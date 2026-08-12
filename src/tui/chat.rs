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
/// Max running agents shown while the compact entity selector is open.
pub const ENTITY_ROWS_MAX: usize = 6;
/// Max running agents shown at once in the background-agent manager.
pub const AGENT_MANAGER_ROWS_MAX: usize = 8;
/// Agent detail follows the reference dialog's bounded prompt preview.
pub const AGENT_PROMPT_CHARS_MAX: usize = 300;
pub const AGENT_PROMPT_ROWS_MAX: usize = 6;
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
pub(crate) enum EditKind {
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
    pub(crate) events_rx: mpsc::UnboundedReceiver<UiEvent>,
    pub(crate) asks_rx: mpsc::UnboundedReceiver<AskRequest>,
    pub messages: Vec<UiMessage>,
    pub input: String,
    /// Byte position of the caret in `input` (always on a char boundary).
    pub cursor: usize,
    /// Text last deleted with ctrl+k/u/w (ctrl+y pastes it back).
    pub(crate) kill: String,
    /// Edit undo stack (text + caret), capped at [`UNDO_MAX`].
    pub(crate) undo: Vec<(String, usize)>,
    /// Type of the last edit (consecutive same-kind edits merge in the undo stack).
    pub(crate) last_edit: Option<EditKind>,
    /// Thinking level before Alt+T disabled it (pressing again restores it).
    pub(crate) last_thinking: Option<String>,
    /// Input stashed with ctrl+s (text + caret).
    pub(crate) stash: Option<(String, usize)>,
    /// Submitted prompts (persisted per cwd; falls back to in-session on write failure).
    pub history: crate::tui::history::History,
    /// Whether the history file is writable (after one failure, never retry — avoid hitting the same error on every submit).
    pub(crate) history_writable: bool,
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
    pub(crate) ctrl_c_at: Option<std::time::Instant>,
    /// Time the running turn was first asked to stop; cleared when the next turn starts.
    /// Ctrl+C force-quits once it is older than [`INTERRUPT_GRACE`] and the turn is still busy.
    pub(crate) interrupt_at: Option<std::time::Instant>,
    /// Time of the most recent Esc (a second press within [`ESC_WINDOW`] clears the input).
    pub(crate) esc_at: Option<std::time::Instant>,
    /// Time of the last key press and the count of consecutive "fast" keys (paste-burst heuristic).
    pub(crate) last_key_at: Option<std::time::Instant>,
    pub(crate) burst_keys: usize,
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
    /// Current response-attempt start within the live message. Retrying restores this snapshot,
    /// preserving completed tool rounds even when the failed attempt mutated an existing group.
    stream_attempt_checkpoint: Option<UiMessage>,
    /// Message opened by [`Chat::open_continuation_message`] to carry what the model says after a
    /// mid-turn answer. Recorded so a turn that ends without using it can drop it again —
    /// inferring that from "empty assistant message" would also catch messages nobody opened here.
    pub(crate) continuation_msg: Option<usize>,
    pub(crate) thinking_buf: String,
    /// Whether the current thinking segment is open for continuation: closed after ToolStart/TextDelta
    /// (segment boundaries); deltas in the same segment continue without paragraph breaks; new segments (fresh reasoning after a tool) are aggregated with \n\n.
    pub(crate) thinking_seg_open: bool,
    pub(crate) output_tokens: u64,
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
    pub(crate) ask_focus: usize,
    /// Buffer for Other free-form input.
    pub(crate) ask_other: String,
    /// Task-list disk snapshot cache (refreshed each tick).
    pub(crate) tasks_cache: Vec<TodoItem>,
    pub(crate) processor: MarkdownProcessor,
    pub(crate) renderer: MarkdownRenderer,
    pub(crate) reply_cache: HashMap<String, Vec<Line>>,
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
    pub(crate) chat_avatars: bool,
    /// Loaded image cache (url → PNG bytes + cell dimensions).
    pub images: HashMap<String, Arc<ImageMeta>>,
    /// Image urls currently being fetched (prevents duplicate loads).
    pub(crate) images_pending: HashSet<String>,
    /// Image urls whose load failed (rendered with a failure marker; a retry
    /// on a later message clears the mark).
    pub(crate) images_failed: HashSet<String>,
    /// Image cache version (bumped on load completion → invalidates the render cache).
    pub(crate) images_version: u64,
    /// Whether the document needs rebuilding (set after writes like events/tick/expand; cleared after the layout layer consumes it).
    pub dirty: bool,
    /// Width of the last build_rows (markdown cache invalidated by width).
    pub(crate) prev_build_width: usize,
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
    pub(crate) pending_tools: Vec<usize>,
    pub theme: Theme,
    /// Detected terminal background color (used by /theme to rebuild the theme).
    detected_background: Option<bool>,
    /// Update banner (welcome card): the latest detected version (`vX.Y.Z`; None = no banner row).
    /// Data source: `crate::update::latest_cached` (24h TTL cache, warmed at startup).
    pub update_banner: Option<String>,
    /// Breathing animation start tick (window = [`UPDATE_BANNER_FRAMES`] frames; current frame = tick − start).
    pub(crate) update_banner_start: u64,
    /// Animation stopped (triggered by the first keypress in the window; the banner stays, it just stops breathing).
    pub(crate) update_banner_stopped: bool,
    /// motion off (settings `motion:"off"` or `BINGO_NO_MOTION=1`): breathing rests at the rest color
    /// and the banner stays (update-banner spec §2.5 "the indicator does not disappear, it just stops").
    pub(crate) motion_off: bool,
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
    pub(crate) mark_base: usize,
    /// slash dropdown suggestions (non-empty when the input is `/` without arguments; rendered by the component layer).
    pub slash_suggestions: Vec<SlashSuggestion>,
    /// Selected index in the dropdown.
    pub slash_selected: usize,
    /// `/model` two-level selector (level-one endpoint → level-two model list; None = inactive).
    pub model_menu: Option<ModelMenu>,
    /// Last-used model per provider (session memory): switching back to a
    /// provider restores what you used there.
    pub(crate) provider_models: std::collections::HashMap<String, String>,
    /// Current provider was chosen session-only (`s` in the picker): model
    /// changes stay session-only too instead of half-persisting a pair.
    pub(crate) provider_session_only: bool,
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
    /// Snapshot of the bottom entity area (running agent instances + channels; refreshed on tick/WatchEvent).
    pub entities: Vec<EntityRow>,
    /// Selection in the compact running-agent list; `None` keeps the one-line presence summary.
    pub entity_focus: Option<usize>,
    /// Main-view background-agent manager; `None` means the panel is closed.
    pub agent_manager: Option<AgentManager>,
    /// Entity view pending open (app layer consumes → enters the fullscreen modal).
    pub open_entity: Option<EntityOpen>,
    /// Slack workspace view state. Lives here rather than in the modal so read
    /// cursors, the open conversation and collapsed sections survive leaving
    /// and re-entering the view.
    pub slack: crate::tui::slack::Workspace,
    /// Interrupt signal: Ctrl+C / Esc while busy → send(true), aborting stream reads in the turn immediately.
    pub(crate) cancel_tx: tokio::sync::watch::Sender<bool>,
}

/// One row of the bottom entity area: a subagent instance or a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityRow {
    Agent {
        name: String,
        state: &'static str,
        model: String,
        thinking: Option<String>,
    },
    Channel {
        name: String,
        seq: u64,
        frozen: bool,
    },
}

/// Background-agent manager layered over the main chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentManager {
    List { selected: usize },
    Detail { name: String },
}

/// Entity view to open from the main chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityOpen {
    Workspace,
    Agent(String),
}

impl Chat {
    /// Display TTL for non-fatal warnings: expired entries are no longer
    /// rendered (pruned on push).
    const WARNING_TTL: std::time::Duration = std::time::Duration::from_secs(10);

    /// Record a non-fatal warning (de-duped + stale entries pruned).
    pub(crate) fn push_warning(&mut self, message: String) {
        self.warnings
            .retain(|(t, _)| t.elapsed() < Self::WARNING_TTL);
        if message.starts_with(crate::query::RECONNECT_WARNING_PREFIX) {
            self.warnings.retain(|(_, warning)| {
                !warning.starts_with(crate::query::RECONNECT_WARNING_PREFIX)
            });
        }
        if !self.warnings.iter().any(|(_, w)| w == &message) {
            self.warnings.push((std::time::Instant::now(), message));
        }
    }

    /// The warning currently displayed (`None` when nothing is
    /// unexpired).
    pub fn visible_warning(&self) -> Option<&str> {
        self.warnings
            .iter()
            .rev()
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
            stream_attempt_checkpoint: None,
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
            agent_manager: None,
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
                self.stream_attempt_checkpoint = self
                    .stream_msg
                    .and_then(|index| self.messages.get(index).cloned());
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
            UiEvent::StreamRetry => {
                if let Some(index) = self.stream_msg {
                    if let Some(checkpoint) = self.stream_attempt_checkpoint.clone() {
                        self.messages[index] = checkpoint;
                    }
                    let text_len = self.messages[index].text.chars().count();
                    let mut hint = Activity::new(ActivityKind::Thinking(Thinking {
                        state: ThinkingState::Running,
                        duration_ms: 0,
                        stage: thinking_stage(self.messages.len()),
                        done_verb: Some(thinking_done_verb()),
                        start_tick: self.tick,
                        segments: 1,
                    }));
                    hint.expand_hint = Some("ctrl+o to expand".to_string());
                    self.messages[index].activities.push(hint);
                    self.messages[index].insert_points.push(text_len);
                    self.messages[index].group_of.push(None);
                }
                self.thinking_buf.clear();
                self.thinking_seg_open = false;
                self.pending_tools_clear();
                self.output_round_tokens = 0;
                self.token_rate.retry_round();
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
                    self.stream_attempt_checkpoint = self.messages.get(i).cloned();
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
                self.stream_attempt_checkpoint = None;
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
                    self.stream_attempt_checkpoint = None;
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
    pub(crate) fn collapse_paste(&mut self) {
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
    pub(crate) fn resolve_images(&self, text: &str) -> Vec<crate::api::types::ImageAttachment> {
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
    pub(crate) fn session_for_turn(&self) -> Arc<Session> {
        if self.permission_mode == self.session.permission_mode {
            return self.session.clone();
        }
        let mut session = (*self.session).clone();
        session.permission_mode = self.permission_mode;
        Arc::new(session)
    }

    /// Queues slash output lines (transient hints: rendered after messages and above the input, gone after TTL).
    pub(crate) fn push_slash_output(&mut self, text: String) {
        for line in text.lines() {
            self.slash_lines.push(line.to_string());
        }
        self.slash_at = Some(std::time::Instant::now());
        self.dirty = true;
    }

    /// Queues slash error/usage rows (G12): they live longer than success hints
    /// ([`SLASH_OUTPUT_ERROR_TTL`] floor) and clear on the next input — the user needs
    /// time to read "what happened + what you can do" (feedback-states §3).
    pub(crate) fn push_slash_error(&mut self, text: String) {
        for line in text.lines() {
            self.slash_error_lines.push(line.to_string());
        }
        self.slash_error_at = Some(std::time::Instant::now());
        self.dirty = true;
    }

    /// Informational output tier: persists until the next input or Esc (no
    /// TTL) — for content the user explicitly asked to read.
    pub(crate) fn push_slash_info(&mut self, text: String) {
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
    pub(crate) fn menu_open(&self) -> bool {
        self.model_menu.is_some()
            || self.think_menu.is_some()
            || self.theme_menu.is_some()
            || self.resume_menu.is_some()
            || self.provider_menu.is_some()
    }

    /// The single mutual-exclusion point: every open_* goes through here —
    /// the old per-open hand-written clears formed an asymmetric triangle
    /// (newer menus closed older ones, never the reverse).
    pub(crate) fn close_menus(&mut self) {
        self.model_menu = None;
        self.think_menu = None;
        self.theme_menu = None;
        self.resume_menu = None;
        self.provider_menu = None;
        self.dirty = true;
    }

    /// Clears the slash dropdown and its no-match flag together (single lifecycle).
    pub(crate) fn clear_slash_suggestions(&mut self) {
        self.slash_suggestions.clear();
        self.slash_no_match = false;
    }

    /// Slash command dispatch. Returns true = consumed.
    pub(crate) fn run_slash(&mut self, line: &str) -> bool {
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
            "gc" => self.slash_gc(),
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

    fn rebind_tasks_to_transcript(&self, transcript: Option<&crate::transcript::Transcript>) {
        let key = transcript
            .map(crate::transcript::Transcript::name)
            .filter(|key| !key.is_empty())
            .unwrap_or_else(|| crate::tasks::project_task_key(&self.session.cwd()));
        self.session.tasks.rebind(&key);
    }

    fn attach_share_to_transcript(&mut self, transcript: Option<&crate::transcript::Transcript>) {
        self.session.agents.detach_share();
        self.session.channels.detach_share();
        let Some(transcript) = transcript else {
            return;
        };
        let path = crate::share::shares_dir(&self.session.home)
            .join(format!("{}.json", transcript.name()));
        match crate::share::ShareStore::load_or_create(&path) {
            Ok(store) => {
                self.session.channels.align_with_share(&store);
                self.session.agents.attach_share(store.clone());
                self.session.channels.attach_share(store);
            }
            Err(error) => self.push_warning(format!(
                "share store unavailable ({error}); bingo share will have the conversation view only"
            )),
        }
    }

    fn slash_clear(&mut self) {
        let session = self.session.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let new_transcript = crate::transcript::create(&session.home, &cwd).ok();
        let _ = session.runtime.transcript_tx.send(new_transcript.clone());
        self.rebind_tasks_to_transcript(new_transcript.as_ref());
        self.attach_share_to_transcript(new_transcript.as_ref());
        self.messages.clear();
        self.stream_msg = None;
        self.stream_attempt_checkpoint = None;
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
    pub(crate) fn persist_selection(&self, model: &str, provider: &str) {
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
    pub(crate) fn provider_order(&self) -> Vec<String> {
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
    pub(crate) fn model_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
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
                if let Some(menu) = self.model_menu.as_mut()
                    && menu.models.is_some()
                {
                    menu.models = None;
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
    pub(crate) fn theme_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
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
        let old_name = t.name();
        match t.rename(arg) {
            Ok(new_t) => {
                let name = new_t.name();
                if let Err(error) = self.session.tasks.rename_key(&old_name, &name) {
                    self.push_warning(format!(
                        "task data could not follow the renamed session ({error}); tasks remain under the previous session name"
                    ));
                }
                if let Err(error) =
                    crate::share::rename_session_sidecars(&self.session.home, &old_name, &name)
                {
                    self.push_warning(format!(
                        "share data could not follow the renamed session ({error}); export may omit agent/channel history"
                    ));
                }
                let _ = self.session.runtime.transcript_tx.send(Some(new_t.clone()));
                self.attach_share_to_transcript(Some(&new_t));
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
        if let Err(error) = found.activate() {
            self.push_slash_error(format!("cannot resume session: {error}"));
            return;
        }
        let count = found.load_messages().unwrap_or_default().len();
        let _ = self.session.runtime.transcript_tx.send(Some(found.clone()));
        self.rebind_tasks_to_transcript(Some(found));
        self.attach_share_to_transcript(Some(found));
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
    pub(crate) fn resume_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
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
                if let Err(error) = t.activate() {
                    self.resume_menu = None;
                    self.push_slash_error(format!("cannot resume session: {error}"));
                    return true;
                }
                let name = t.name();
                let count = t.load_messages().unwrap_or_default().len();
                self.resume_menu = None;
                let _ = self.session.runtime.transcript_tx.send(Some(t.clone()));
                self.rebind_tasks_to_transcript(Some(&t));
                self.attach_share_to_transcript(Some(&t));
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

    fn slash_gc(&mut self) {
        if self.busy {
            self.push_slash_error(format!(
                "[error] code={} msg=cannot clean session data mid-turn (press Esc to interrupt, then retry)",
                crate::error::SLASH_ERROR_BAD_ARGUMENT
            ));
            return;
        }
        let home = self.session.home.clone();
        let protected = self
            .session
            .runtime
            .transcript
            .borrow()
            .as_ref()
            .map(|transcript| transcript.path().to_path_buf());
        self.pin_panel("gc", vec!["⏳ cleaning session data…".to_string()]);
        let result = crate::storage::cleanup(&home, protected.as_deref());
        self.unpin_panel("gc");
        match result {
            Ok(report) => self.push_slash_info(format!("✓ {}", report.summary())),
            Err(error) => {
                self.last_error = Some(ErrorState {
                    code: crate::error::map_error(&error),
                    msg: format!(
                        "session storage cleanup failed: {error}; check disk permissions and retry /gc"
                    ),
                    level: crate::error::ErrorLevel::Page,
                    context: crate::error::ErrorContext::ShortSync,
                });
                self.dirty = true;
            }
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
}

pub(crate) fn one_line(text: &str, width: usize) -> String {
    let flat = crate::tui::line::sanitize(text);
    crate::tui::markdown::truncate(flat.as_ref(), width.max(1))
}

pub(crate) fn user_message_rows(text: &str, width: usize, theme: &Theme) -> Vec<Row> {
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

pub(crate) fn text_rows(theme: &Theme, reply: Vec<Line>) -> Vec<Row> {
    let claude = theme.claude;
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
        .collect()
}

#[path = "chat_tail.rs"]
mod chat_tail;

#[cfg(test)]
pub(crate) use chat_tail::{banner_line, banner_segments, update_color, welcome_card_rows};

#[cfg(test)]
#[path = "chat_tests_a.rs"]
mod tests_a;

#[cfg(test)]
#[path = "chat_tests_b.rs"]
mod tests_b;
