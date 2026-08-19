//! Incremental model for the chat state machine: messages/activities/collapse groups + document row construction.
//!
//! Ported from the old `tui.rs` `BingoChat` (ratatui edition): event handling semantics,
//! collapse detection, and expand/collapse toggling are preserved as-is; `draw` is replaced by [`Chat::build_rows`],
//! which builds transcript blocks ([`crate::tui::statics::Block`]) laid out by
//! [`crate::tui::statics::layout`] into display-agnostic styled row documents, mapped to
//! terminal rows by [`crate::tui::view`].
//! Events arrive from channels (`UiEvent`); keyboard/mouse come in via
//! [`Chat::on_key`] / [`Chat::doc_click`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::Color;
use rsmarkdown_core::{MarkdownProcessor, Renderer};
use tokio::sync::mpsc;

use crate::budget::MAX_RESULT_CHARS;
use crate::permission::PermissionMode;
use crate::query::Session;
use crate::tui::activities::{
    Activity, ActivityKind, Diff, Portrait, Thinking, ThinkingState, TodoItem, TodoStatus,
    ToolCall, ToolStatus, WatchCall, activities_path_get_mut, diff_lines, layout_activity,
};
use crate::tui::avatar;
use crate::tui::gfx::{self, ImageCap};
use crate::tui::line::{Line, SegStyle, text_width, wrap_words};
use crate::tui::markdown::MarkdownRenderer;
use crate::tui::notify::{Attention, Notifier, Title};
use crate::tui::theme::{Theme, ThemeSetting};
use crate::ui::{ImageMeta, PermissionRequest, UiEvent};
use crate::watch::WatchState;

pub use crate::tui::el::{ClickTarget, Row};
pub use crate::tui::statics::{Doc, SettledMark};

use crate::tui::el::{El, LocalClick};
use crate::tui::statics::Block;

/// Current error state (#18 presentation layer): `code`/`msg`/`level`/`context` come from structured
/// `UiEvent::Error`; the level is decided by the triggering context (short sync = page-level, long turn = full-flow).
#[derive(Debug, Clone)]
pub struct ErrorState {
    pub code: String,
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
    /// Wall-clock send time, unix seconds (0 = no clock; renders no stamp).
    /// User messages stamp at submit; assistant messages restamp at turn end,
    /// so the shown time is when the reply landed, as in the workspace views.
    pub at: u64,
    /// Who said it, when the marker walk cannot tell (v6): an away page's
    /// messages carry their speaker explicitly, because a room's or an agent's
    /// counterparts are not derivable from the text the way main's two
    /// participants are. `None` falls back to the transcript's own reading
    /// (`speaker_of`), so main's flow is byte-identical.
    pub speaker: Option<String>,
    pub activities: Vec<Activity>,
    /// Char count of text at activities[i] creation: rendering interleaves text and activities in model output order.
    pub insert_points: Vec<usize>,
    /// Collapse groups for consecutive Read/Search operations.
    pub groups: Vec<CollapseGroup>,
    /// Index of the collapse group activities[i] belongs to (None = standalone activity).
    pub group_of: Vec<Option<usize>>,
}

// moved to [`crate::tui::collapse`] with the rest of the fold machinery
// (D111, the 4000-line cap); re-exported because a group is part of a
// message's own state and every consumer already speaks this path.
pub use crate::tui::collapse::{CollapseGroup, CollapseKind, classify_tool, collapse_summary};

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

pub use crate::tui::slash::SlashSuggestion;

/// Footer model badge: `{model} · think {level}` (off = no level shown, keeps it concise).
pub fn model_footer_label(model: &str, thinking: Option<&str>) -> String {
    match thinking {
        Some(level) if level != "off" => format!("{model} · think {level}"),
        _ => model.to_string(),
    }
}

use crate::tui::model_menu::ModelMenu;

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
            think_levels()
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
/// The `/theme` vocabulary, from the one command table (D146). The picker draws
/// the action's own argument choices rather than a second list beside them.
pub fn theme_levels() -> crate::app::action::Choices {
    crate::app::action::choices_for("theme.set", "theme")
}

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
            theme_levels()
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

/// `/images` picker (D97): the session's content images, newest first.
///
/// The thinnest shell in the family — no `current`, because there is no image
/// "in effect"; the list is a history and Enter is an action on one of its
/// entries rather than a setting being chosen.
#[derive(Clone)]
pub struct ImagesMenu {
    /// Browsed index (❯): moves with ↑↓/1-9.
    pub selected: usize,
    /// Registry ids in the order the picker shows them, so Enter opens the
    /// image the row named even after the registry has grown underneath.
    pub ids: Vec<usize>,
    /// The rows, pre-rendered at open time for the same reason.
    pub items: Vec<crate::tui::picker::PickerItem>,
}

impl ImagesMenu {
    pub fn picker(&self) -> crate::tui::picker::PickerModel {
        crate::tui::picker::PickerModel::new(self.items.clone(), self.selected, None)
    }

    /// Number jump, no session-only: opening a viewer is not a setting.
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
/// The `/think` vocabulary, from the one command table (D146).
pub fn think_levels() -> crate::app::action::Choices {
    crate::app::action::choices_for("thinking.select", "level")
}

/// Max visible rows in the dropdown (OVERLAY_MAX_ITEMS = 5).
pub const SLASH_SUGGESTIONS_MAX: usize = 5;

/// Max rows rendered for the input area (longer input scrolls to the caret's line).
pub const INPUT_ROWS_MAX: usize = 10;
/// Max rows shown for queued messages (more collapse into `… +N more`).
pub const QUEUE_ROWS_MAX: usize = 3;
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

/// The line a dialog leaves behind when the turn that asked it ended first
/// (D80). Display-only: the model's history never carries it, because the tool
/// call that opened the dialog went down with the turn — this is the record for
/// the user, in the place the dialog used to be.
pub const ASK_CANCELLED_TEXT: &str = "(pending permission dialog cancelled with the turn)";

/// The receipt a resolved permission dialog leaves in the flow: the choice the
/// user made, in the place the dialog was (D81). The dialog itself is chrome and
/// disappears with the answer; without this the transcript would show a turn
/// that simply carried on, with no record of who let it.
pub const ASK_RECEIPT_YES: &str = "> yes";
pub const ASK_RECEIPT_SESSION: &str = "> yes, don't ask again this session";
pub const ASK_RECEIPT_NO: &str = "> no";
/// A refusal that carried feedback: `> no — <what to do instead>`.
pub const ASK_RECEIPT_NO_PREFIX: &str = "> no — ";

/// One line of an action's own report, as the terminal shows it.
///
/// The tier comes from the work rather than from here, which is the point: the
/// core records the same sentence as a notice item, so a GUI and this screen say
/// the same thing about the same outcome without either owning the words.
pub(crate) fn said_event(said: crate::engine::actions::Said) -> UiEvent {
    use crate::engine::actions::Tier;
    match said.tier {
        Tier::Error => UiEvent::SlashError(said.text),
        Tier::Info => UiEvent::SlashInfo(said.text),
        Tier::Output => UiEvent::SlashOutput(said.text),
    }
}

/// Whether a line is a permission receipt. Matched whole for the three plain
/// choices and by the em-dash prefix for a refusal with feedback — a bare `> `
/// test would swallow every markdown quote the user ever pastes.
pub(crate) fn is_ask_receipt(text: &str) -> bool {
    matches!(text, ASK_RECEIPT_YES | ASK_RECEIPT_SESSION | ASK_RECEIPT_NO)
        || text.starts_with(ASK_RECEIPT_NO_PREFIX)
}

/// A user-role message the user never wrote: the harness recorded it to state
/// what happened. State lines render as a single line — no `❯` bubble putting
/// words in the user's mouth, and no send stamp, because nothing was sent.
pub(crate) fn is_state_line(text: &str) -> bool {
    crate::query::is_interrupt_marker(text)
        || text == ASK_CANCELLED_TEXT
        || is_ask_receipt(text)
        || crate::tui::bufferview::is_agent_alert(text)
        || crate::tui::bufferview::is_agent_notice(text)
        || rewind_ui::is_rewind_line(text)
}

/// A message the running turn absorbed mid-turn (D83). Not a state line: the user wrote
/// it and it reached the model, so it keeps its send stamp — it only loses the `❯`
/// bubble, because the `↪` glyph is what marks where in the reply it landed.
pub(crate) fn is_steer_line(text: &str) -> bool {
    text.starts_with(crate::app::queue::STEER_FLOW_PREFIX)
}

/// Hint shown while a collapse group runs: the input of the group's most recent tool.
/// File a just-readied tool into its message's collapse machinery: the shared
/// half of the live `ToolReady` path and the away page's rehydrate (v6). The
/// activity at `idx` is already pushed; this decides whether it joins the open
/// group, opens a new one, or breaks the fold.
pub(crate) fn group_ready_tool(
    msg: &mut UiMessage,
    idx: usize,
    name: &str,
    input: &serde_json::Value,
) {
    let kind = classify_tool(name, input);
    let Some(kind) = kind else {
        if let Some(g) = msg.groups.last_mut() {
            g.active = false;
        }
        return;
    };
    let open = msg
        .groups
        .last()
        .is_some_and(|g| g.active && !g.activities.is_empty());
    let g = if open {
        msg.groups.len() - 1
    } else {
        msg.groups.push(CollapseGroup {
            active: true,
            ..CollapseGroup::default()
        });
        msg.groups.len() - 1
    };
    msg.group_of[idx] = Some(g);
    msg.groups[g].activities.push(idx);
    msg.groups[g].last_hint = Some(hint_for(name, input));
    match kind {
        CollapseKind::Search => msg.groups[g].search += 1,
        CollapseKind::Read(path) => match path {
            Some(p) => msg.groups[g].read_paths.push(p),
            None => msg.groups[g].read_ops += 1,
        },
        CollapseKind::List => msg.groups[g].list += 1,
        CollapseKind::Bash => msg.groups[g].bash += 1,
        CollapseKind::AgentCheck => msg.groups[g].agent_checks += 1,
        CollapseKind::AgentStop => msg.groups[g].agent_stops += 1,
        CollapseKind::AgentDelete => msg.groups[g].agent_deletes += 1,
        CollapseKind::Send(target) => msg.groups[g].send_targets.push(target),
        CollapseKind::RoomCheck => msg.groups[g].room_checks += 1,
        CollapseKind::RoomCreate => msg.groups[g].room_creates += 1,
        CollapseKind::RoomRoster => msg.groups[g].room_rosters += 1,
    }
}

pub(crate) fn hint_for(name: &str, input: &serde_json::Value) -> String {
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
/// The next permission mode in the shift+tab ladder (CC `app:cycleMode`).
///
/// `startup` is the mode the process was launched in, and it is what makes the
/// dangerous modes reachable without being *introduced*: a session that never
/// started in bypass/dontAsk can never cycle into one.
///
/// Pure since D105, because the zoom cycles a *different* subject's mode — the
/// viewed agent's — and CC does exactly that, calling its own
/// `getNextPermissionMode` on the teammate's context and leaving the leader's
/// alone (`PromptInput.tsx:1410-1447`).
/// The permission mode as the core spells it. The two vocabularies are the same
/// five names; the crossing lives here so nothing else has to know both.
pub fn app_permission_mode(mode: PermissionMode) -> crate::app::snapshot::PermissionMode {
    use crate::app::snapshot::PermissionMode as App;
    match mode {
        PermissionMode::Default => App::Default,
        PermissionMode::AcceptEdits => App::AcceptEdits,
        PermissionMode::BypassPermissions => App::BypassPermissions,
        PermissionMode::DontAsk => App::DontAsk,
        PermissionMode::Plan => App::Plan,
    }
}

/// And back, for a console reading the mode the core holds.
pub fn console_permission_mode(mode: crate::app::snapshot::PermissionMode) -> PermissionMode {
    use crate::app::snapshot::PermissionMode as App;
    match mode {
        App::Default => PermissionMode::Default,
        App::AcceptEdits => PermissionMode::AcceptEdits,
        App::BypassPermissions => PermissionMode::BypassPermissions,
        App::DontAsk => PermissionMode::DontAsk,
        App::Plan => PermissionMode::Plan,
    }
}

pub fn next_permission_mode(mode: PermissionMode, startup: PermissionMode) -> PermissionMode {
    let next = match mode {
        PermissionMode::Default => PermissionMode::AcceptEdits,
        PermissionMode::AcceptEdits => PermissionMode::Plan,
        PermissionMode::Plan => PermissionMode::Default,
        // Started in bypass/dontAsk: toggle between it and default, never introducing a new dangerous mode.
        PermissionMode::BypassPermissions | PermissionMode::DontAsk => PermissionMode::Default,
    };
    // From default, switch back to the startup mode (an edge that only bypass/dontAsk sessions have).
    if next == PermissionMode::AcceptEdits
        && matches!(
            startup,
            PermissionMode::BypassPermissions | PermissionMode::DontAsk
        )
    {
        return startup;
    }
    next
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

/// Skill's result row: only `✦ <skill name>`, the same family as the activity
/// header `✦ Skill(input)`. The pointer path the tool returns stays in
/// `tool_result`, where the model reads it — a row is not a place for a path.
pub(crate) fn skill_result_summary(output: &str) -> Option<String> {
    output.lines().next().and_then(|line| {
        line.strip_prefix("✦ ")
            .and_then(|rest| rest.split(" — ").next())
            .map(|name| format!("✦ {name}"))
    })
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

/// Expandable content of a finished tool row: the result text, blank lines dropped.
///
/// One rule for every call, grouped or standalone — the fold is a display state, not a
/// different kind of result. The char budget is the one the model already lives under
/// ([`MAX_RESULT_CHARS`]): results reach the UI clipped to it, and applying it here bounds
/// what a row retains even for the paths that build their output without going through the
/// clip (a tool error string, a denied call).
pub(crate) fn result_content(name: &str, output: &str) -> Vec<Line> {
    let bounded = match output.char_indices().nth(MAX_RESULT_CHARS) {
        Some((end, _)) => &output[..end],
        None => output,
    };
    let lines: Vec<String> = bounded.lines().map(str::to_string).collect();
    let preview: Vec<String> = if name == "Bash" {
        bash_output_preview(&lines)
    } else {
        lines
    };
    preview
        .into_iter()
        .filter(|l| !l.trim().is_empty())
        .map(Line::plain)
        .collect()
}

/// Playful words for the thinking stage.
pub(super) const THINKING_WORDS: [&str; 12] = [
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

/// What the console does once the addressed store is back in place.
///
/// These reactions read a conversation themselves — they start turns, drain the
/// queue and write into main's transcript — so none of them can run while a store
/// is detached for the length of a handler.
#[derive(Default)]
struct Follow {
    /// A finished background run left a notification in main's context: ask the
    /// core to wake a turn to read it.
    wake: bool,
    /// A run that failed, named, with its reason (D98).
    alert: Option<(String, Option<String>)>,
    /// A run that finished and reported itself to main (D106).
    notice: Option<String>,
    /// A turn that died on its own (error, lost task) leaves its dialog behind
    /// exactly as an interrupt did; the receiver is already gone, which is what
    /// tells it apart from a background agent's question (D80). It writes the
    /// cancelled line into main's transcript, so it waits for the store.
    settle_asks: bool,
}

/// bingo chat component state: message stream + activity notices + input + permission requests.
pub struct Chat {
    pub session: Arc<Session>,
    /// What a keystroke asked the core for, waiting for the loop to perform it
    /// (D154). A key handler cannot wait; it records and returns.
    pub(crate) intents: std::collections::VecDeque<crate::tui::intent::Intent>,
    /// Somebody is already draining the queue. Performing an intent folds the
    /// core's stream, and folding it can leave another intent behind.
    pub(crate) draining: bool,
    /// Whether the last mail digest opened a turn.
    pub(crate) digest_woke: bool,
    /// What the core has said, materialized (B7b).
    ///
    /// The console is an attachment like any other: it takes one snapshot cut
    /// and folds the ordered event stream that follows it into a local
    /// projection a key handler and a render pass can read without waiting. A
    /// store with no link holds an empty view rather than a wrong one — an
    /// attachment that has heard nothing holds the same.
    pub store: crate::tui::store::Store,
    /// The console's own sink, bound to main. Every other conversation's
    /// producer holds one bound to itself (`AgentHandle::sink_for`).
    pub(super) events: crate::ui::EventSink,
    pub(crate) events_rx: mpsc::UnboundedReceiver<crate::ui::Addressed>,
    /// The running output estimate per page, in units, for the round that is
    /// streaming. The provider's own count replaces it whenever one arrives;
    /// until then this is the only number the footer's rate meter has.
    pub(crate) stream_units: HashMap<crate::ui::ConvKey, u64>,
    /// The conversation on screen. Everything the running turn writes lives
    /// here; everything the console owns stays on `Chat`.
    pub conv: crate::tui::conversation::Conversation,
    /// Every conversation that is *not* on screen, keyed. Main is in here like
    /// anybody else whenever the screen is somewhere else — which is the D134
    /// ruling made storage: switching pages is one chrome pointed at a different
    /// store, and main differs only in talking to the user by default.
    ///
    /// Keeping the active one inline rather than in the map is what makes
    /// [`Chat::conv`] infallible: the renderer, the composer and the status row
    /// read it on every frame, and a lookup that could miss would be a panic
    /// path in all of them.
    pub(crate) parked: HashMap<crate::ui::ConvKey, crate::tui::conversation::Conversation>,
    /// Which conversation `conv` is.
    pub(crate) active: crate::ui::ConvKey,
    pub input: String,
    /// Byte position of the caret in `input` (always on a char boundary).
    pub cursor: usize,
    /// Readline state for the prompt: the kill ring alt+k/ctrl+u/ctrl+w/alt+d feed and
    /// ctrl+y/alt+y read, plus the `ctrl+x ctrl+e` chord (D86).
    pub(crate) composer: crate::tui::composer::Composer,
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
    /// Foreground command liveness (D84): the seam the running Bash tool publishes
    /// its output tail through, and the one ctrl+b reaches to background it.
    pub(crate) live: std::sync::Arc<crate::live::LiveBash>,
    /// The tail of the command running right now (None: no foreground command, or
    /// it has not written anything yet). One slot, because Phase 2 runs non-safe
    /// tools serially and Bash is never concurrency-safe.
    pub(crate) bash_tail: Option<crate::live::LiveTail>,
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
    /// This edit arrived as a paste rather than as typing — a detected burst
    /// key or a bracketed `Event::Paste`. The completion surfaces read it and
    /// close instead of recomputing (D86).
    pub(crate) pasting: bool,
    /// Collapsed paste blocks: placeholder `[Pasted text #N +M lines]` → real content.
    pastes: Vec<(String, String)>,
    /// `!` commands run in this session (prefix completion for Tab in bash mode).
    bash_history: Vec<String>,
    /// ctrl+r reverse search state (None = not active).
    pub search: Option<HistorySearch>,
    /// ctrl+l requests a full-screen repaint (cleared after the render layer consumes it).
    pub force_redraw: bool,
    /// ctrl+o requests the transcript view: the host opens the alternate-screen
    /// pager over the whole session (cleared after consumption, D82).
    pub open_transcript: bool,
    /// ctrl+g (or `ctrl+x ctrl+e`) requests the `$EDITOR` compose: the host
    /// hands the terminal over and puts the edited draft back (cleared after
    /// consumption, D86).
    pub open_editor: bool,
    /// The conversation the zoomed view is on, while it has the screen. `None`
    /// is the transcript, which is every frame the inline host draws.
    pub(crate) zoom: Option<crate::tui::zoom::ZoomTarget>,
    /// A page was just turned: the inline host owes the terminal one
    /// [`crate::tui::term::InlineTerm::page_break`] before the next frame
    /// (the fullscreen host redraws whole and only clears the flag).
    pub page_turn: bool,
    /// bash mode (`!` prefix): input executes directly, bypassing the model.
    pub bash_mode: bool,
    pub tick: u64,
    /// The digest debounce's state while mail is waiting (D98). `None` means the
    /// main agent's inbox is empty and there is nothing to wait out.
    ///
    /// Console state despite naming an agent (D133): it gates *when the console
    /// starts a turn*, not what any conversation contains. D136 is where that
    /// stops being true — once main is an ordinary registry entry its mail is an
    /// inbox like everyone else's, and this window retires with it.
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
    /// The prompt on screen: the core's identity for it, and the render model
    /// built from it. The answer belongs to the actor (B3); this is the view.
    pub pending_ask: Option<(crate::app::ids::InteractionId, PermissionRequest)>,
    /// The last prompt this console adopted from the projection.
    ///
    /// A client acts on a prompt before it is told the prompt closed, so for a
    /// tick the projection still holds one the console has already settled.
    /// This is what keeps it from picking the same one up again.
    pub(crate) last_ask: Option<crate::app::ids::InteractionId>,
    /// Dialog focus row (0..=options.len(); == options.len() = free-text input).
    pub(crate) ask_focus: usize,
    /// Buffer for free-form input: AskUserQuestion's Other, or a refusal's feedback.
    pub(crate) ask_other: String,
    /// ctrl+e: the pre-approval preview is showing in full, not bounded.
    pub(crate) ask_expanded: bool,
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
    pub(crate) faces_pinned: HashMap<String, usize>,
    /// A subagent's portrait on its watch row (`experimental.chatAvatars`, off by
    /// default): with it on, `◉ scout · task` wears scout's face instead of the
    /// glyph.
    ///
    /// **The sender band retired with D99.** The band existed because the console
    /// had no gutter and a face had to go overhead; the console has one now, and
    /// a band would have drawn the same speaker's portrait twice on one message.
    /// So every conversation's faces are unconditional and this switch governs
    /// the one place a portrait still costs something it did not already own.
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
    /// Content images the session has shown, newest first, openable in the
    /// desktop's viewer (D97). Avatars are chrome and never land here.
    pub(crate) image_registry: crate::tui::images::ImageRegistry,
    /// The command that opens an image; `None` means the desktop's own handler
    /// ([`crate::tui::images::desktop_opener`]). A value here is the seam the
    /// acceptance tests spawn through, the way the editor command is a value
    /// for [`crate::tui::composer::compose_with`].
    pub(crate) image_opener: Option<String>,
    /// The `/images` picker, when it is open.
    pub(crate) images_menu: Option<ImagesMenu>,
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
    /// The motion gate and its seven tokens (D87). Resolved once from settings
    /// (`motion: "off"` / `BINGO_NO_MOTION=1`); every animated surface asks this
    /// rather than the raw tick, so the gate is honoured in one place instead of
    /// two out of five.
    pub(crate) motion: crate::tui::motion::Motion,
    /// Tick of the last event that reached the TUI. `stall` measures from it:
    /// three seconds of silence mid-turn is worth saying out loud.
    pub(crate) last_progress_tick: u64,
    /// Eased token counter for the status row (D87 `meter`): a count that jumps
    /// by hundreds mid-stream reads as a glitch, so the display travels to it.
    pub(crate) token_meter: crate::tui::motion::Meter,
    /// Attention channel (D79): builds the bell / notification OSC / terminal
    /// title bytes the host collects after each frame. Silent by default —
    /// only [`Chat::set_notifier`] gives it a channel.
    pub notify: Notifier,
    /// The conversation engine (D88): every conversation as one shape. Main
    /// is buffer 0 and the active one; DM / channel / team accounting shadows
    /// the domain here so D89 can switch onto it. Nothing renders from it yet.
    pub(crate) buffers: crate::tui::buffer::Buffers,
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
    /// CC's `isTranscriptMode` (`components/messages/UserTeammateMessage.tsx:139`):
    /// the document is being built for the `ctrl+o` pager rather than for the
    /// flow, so the rows that keep a body folded away may show it. Set and
    /// restored by [`crate::tui::transcript::transcript_rows`] around one build,
    /// exactly as the fold state is — the inline document never sees it true, so
    /// nothing it flushed can disagree with what it flushes next.
    pub(crate) transcript_mode: bool,
    /// slash dropdown suggestions (non-empty when the input is `/` without arguments; rendered by the component layer).
    pub slash_suggestions: Vec<SlashSuggestion>,
    /// Selected index in the dropdown.
    pub slash_selected: usize,
    /// Argument phase (D85): byte offset in [`Chat::input`] where the partial
    /// argument starts. `Some` means the dropdown is offering *values* for a
    /// command rather than command names — rendering drops the `/` prefix and
    /// completion splices at this offset instead of replacing the whole line.
    pub slash_arg_start: Option<usize>,
    /// The `@` mention dropdown (D85); `None` means it is closed.
    pub mention: Option<crate::tui::complete::MentionState>,
    /// Esc dismissed the mention dropdown: it stays closed until the caret
    /// leaves the `@` token, so the next keystroke does not reopen it.
    pub mention_dismissed: bool,
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
    /// The background dialog (D107), `ctrl+b`'s second meaning: agents, shells
    /// and rooms in one modal. `None` means it is closed. It holds a cursor and
    /// a detail pointer and nothing else — every row is rebuilt from the
    /// registries at draw time.
    pub(crate) dialog: Option<crate::tui::background::BackgroundDialog>,
    /// The roster's cursor (v6): `None` is the composer, `Some(i)` a row of
    /// the conversation list under it. Entered by `↓` at the bottom of
    /// history (the CC fallthrough), never by a chord.
    pub(crate) roster_sel: Option<usize>,
    /// The badge fingerprint the slow poll last painted (D115): one entry per
    /// conversation, its unread and its mention bit. See `observe_badges`.
    pub(crate) badge_print: Vec<(crate::tui::buffer::BufferId, u64, bool)>,
    /// Direct messages to main since the sender's zoom was last visited,
    /// per sender (D114). The flow no longer prints an arrival line — the
    /// delivery underneath is untouched, main reads its inbox exactly as
    /// before — so this mirror is what the status layer's dot is made of:
    /// the sender's pill and tree row brighten until the user looks.
    pub(crate) agent_mail: std::collections::HashMap<String, u64>,
    /// The esc-esc rewind selector (D91); `None` means it is closed.
    pub(crate) rewind: Option<rewind_ui::Rewind>,
    /// Interrupt signal: Ctrl+C / Esc while busy → send(true), aborting stream reads in the turn immediately.
    pub(crate) cancel_tx: tokio::sync::watch::Sender<bool>,
}

impl Chat {
    /// Display TTL for non-fatal warnings: expired entries are no longer
    /// rendered (pruned on push).
    const WARNING_TTL: std::time::Duration = std::time::Duration::from_secs(10);

    /// Record a non-fatal warning (de-duped + stale entries pruned).
    pub(crate) fn push_warning(&mut self, message: String) {
        self.warnings
            .retain(|(t, _)| t.elapsed() < Self::WARNING_TTL);
        // A reconnect notice replaces the sender's previous one — 2/10 supersedes
        // 1/10 — but only that sender's: the tier is shared now, and a dedupe
        // that keyed on the prefix alone collapsed main's retry and an
        // instance's into one line that alternated between them (D134a).
        if let Some(at) = message.find(crate::query::RECONNECT_WARNING_PREFIX) {
            let (who, _) = message.split_at(at);
            self.warnings.retain(|(_, warning)| {
                !(warning.starts_with(who)
                    && warning[who.len()..].starts_with(crate::query::RECONNECT_WARNING_PREFIX))
            });
        }
        if !self.warnings.iter().any(|(_, w)| w == &message) {
            self.warnings.push((std::time::Instant::now(), message));
        }
    }

    /// Echo the interrupt marker `query.rs` wrote into the message flow. It is a message,
    /// not a warning: the record it mirrors is permanent, and a 10s toast that expires
    /// while the marker stays in the history is exactly the split-brain this closes.
    pub(crate) fn push_interrupt_marker(&mut self, marker: &str) {
        // The turn's own cleanup still has to run against the message it opened, so the
        // continuation drop happens before the marker lands after it (TurnEnd's second
        // call then finds nothing to do).
        self.main_conv().drop_empty_stream_message();
        self.main_conv().messages.push(UiMessage {
            speaker: None,
            role: Role::User,
            text: marker.to_string(),
            at: crate::channels::now_unix(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        self.dirty = true;
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
        events: crate::ui::EventSink,
        events_rx: mpsc::UnboundedReceiver<crate::ui::Addressed>,
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
                    watch_events.send(UiEvent::WatchEvent {
                        label: ev.label,
                        kind: ev.kind,
                        status: ev.state,
                        detail: ev.detail,
                        duration_ms: ev.elapsed_ms,
                        payload: ev.payload,
                        signal: ev.signal,
                        notifies_main: ev.notifies_main,
                        dispatch: ev.dispatch,
                    });
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
        // Update-banner (welcome card) data source + motion off: computed before the session moves into Self.
        // Store the bare version (rendering adds the `v` prefix in `banner_segments`).
        let update_banner = crate::update::latest_cached(&session.home).map(|v| v.to_string());
        let motion = crate::tui::motion::Motion::from_settings(&session.settings);
        let chat_avatars = session.settings.experimental.chat_avatars;
        let context_tokens = session
            .runtime
            .transcript
            .borrow()
            .clone()
            .and_then(|transcript| transcript.load_messages().ok())
            .map(|messages| crate::compact::estimate_tokens(&session.system, &messages, &[]))
            .unwrap_or(0);
        let context_usage = crate::context_usage::ContextUsage::for_model(
            context_tokens,
            &session.client.models(),
            &session.runtime.model.borrow().clone(),
        );
        // A running command's tail reaches the screen the way every other turn-side
        // fact does: as a UiEvent on the channel the drain loop already wakes for.
        // Nothing else in the TUI has to learn a second way to hear from a tool.
        let tail_events = events.clone();
        let live = crate::live::LiveBash::new(Arc::new(move |tail| {
            tail_events.send(UiEvent::BashTail(tail));
        }));
        Self {
            session,
            // Detached until the host attaches it: building a `Chat` is
            // synchronous and attaching is not, so the console connects on the
            // way into its loop (`Chat::connect_store`).
            intents: std::collections::VecDeque::new(),
            draining: false,
            digest_woke: false,
            store: crate::tui::store::Store::default(),
            events,
            events_rx,
            stream_units: HashMap::new(),
            conv: crate::tui::conversation::Conversation::new(context_usage),
            parked: HashMap::new(),
            active: crate::ui::ConvKey::Main,
            input: String::new(),
            cursor: 0,
            composer: crate::tui::composer::Composer::default(),
            undo: Vec::new(),
            last_edit: None,
            last_thinking: None,
            stash: None,
            history,
            history_writable: true,
            live,
            bash_tail: None,
            help_visible: false,
            notice: None,
            notice_until: None,
            ctrl_c_at: None,
            interrupt_at: None,
            esc_at: None,
            last_key_at: None,
            burst_keys: 0,
            pasting: false,
            pastes: Vec::new(),
            bash_history: Vec::new(),
            search: None,
            force_redraw: false,
            open_transcript: false,
            open_editor: false,
            zoom: None,
            page_turn: false,
            bash_mode: false,
            tick: 0,
            warnings: Vec::new(),
            last_error: None,
            last_prompt: String::new(),
            cwd,
            pending_ask: None,
            last_ask: None,
            ask_focus: 0,
            ask_other: String::new(),
            ask_expanded: false,
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
            image_registry: crate::tui::images::ImageRegistry::default(),
            image_opener: None,
            images_menu: None,
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
            theme,
            detected_background,
            update_banner,
            update_banner_start: 0,
            update_banner_stopped: false,
            motion,
            last_progress_tick: 0,
            token_meter: crate::tui::motion::Meter::default(),
            notify: Notifier::default(),
            buffers: crate::tui::buffer::Buffers::new(),
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
            transcript_mode: false,
            slash_suggestions: Vec::new(),
            slash_selected: 0,
            slash_arg_start: None,
            mention: None,
            mention_dismissed: false,
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
            dialog: None,
            roster_sel: None,
            badge_print: Vec::new(),
            agent_mail: std::collections::HashMap::new(),
            rewind: None,
            cancel_tx: tokio::sync::watch::channel(false).0,
        }
    }

    /// Drains all pending events from the channel. Returns whether any event was handled.
    /// Drains the channel; answers whether anything the *screen* is showing
    /// moved.
    ///
    /// A background instance's stream lands in its own store and changes
    /// nothing on a screen that is not on its page — so it must not repaint,
    /// and above all must not reset the stall baseline: main's status row
    /// turning warning-coloured is a statement about main's turn, and somebody
    /// else's deltas are not evidence against it.
    pub fn drain_events(&mut self) -> bool {
        let mut seen = false;
        let mut any = false;
        while let Ok(addressed) = self.events_rx.try_recv() {
            seen |= self.route(addressed);
            any = true;
        }
        // A local event can ask the core for something — a finished run waking
        // main is the one that matters — and what came back is folded in the
        // same pass rather than a tick later (D154).
        if any {
            self.pump_store();
            seen |= self.drain_frames();
        }
        seen
    }

    /// Install the attention channel and take the terminal title with it (D79).
    /// The host does this once, at startup; everything else leaves the
    /// default-silent notifier alone.
    pub fn set_notifier(&mut self, notifier: Notifier) {
        self.notify = notifier;
        self.notify_idle();
    }

    /// Title the terminal for an idle session — no turn running, nothing
    /// waiting on the user.
    fn notify_idle(&mut self) {
        let cwd = crate::tui::notify::cwd_short(&self.cwd).to_string();
        self.notify.set_title(Title::Idle(&cwd));
    }

    /// Drains all channels. Returns whether there is any new state.
    /// Attach the console's store to its session's core and take the first cut.
    ///
    /// Separate from `new` because reading a cut is `async` and constructing is
    /// not. A console that never calls this reads an empty view — which is what
    /// an attachment that has heard nothing reads.
    pub async fn connect_store(&mut self) -> Result<(), crate::app::AppError> {
        let core = self.session.core.clone();
        self.store.connect(&core, "tui").await
    }

    /// The same, without a runtime: what every synchronous console test uses so
    /// that it reads the projection a real terminal reads (see
    /// [`crate::tui::store::Store::connect_now`]).
    #[cfg(test)]
    pub fn connect_store_now(&mut self) -> Result<(), crate::app::AppError> {
        let core = self.session.core.clone();
        self.store.connect_now(&core, "tui")
    }

    /// Let the core finish saying what a change did, then fold it in.
    ///
    /// The two halves a running console gets from its loop for free. The actor
    /// answers a question before it announces what the answer changed, so a test
    /// that changes the session and then reads the projection has to wait for
    /// both — and a real console does, one tick later.
    #[cfg(test)]
    pub(crate) fn settle_store(&mut self) {
        self.session.core.settle_now();
        self.pump_store();
        self.drain_frames();
    }

    /// Fold in everything the core has said since the last look.
    ///
    /// Synchronous on purpose: it sits on the render path, which cannot wait.
    /// A hole in the stream is only *noticed* here; repairing it is the
    /// `async` half ([`Chat::reconcile_store`]) the event loop runs.
    pub fn pump_store(&mut self) -> bool {
        self.store.pump().changed
    }

    /// Repair the store if the stream showed a hole: read a fresh cut and
    /// replace local state with it, exactly as a wire client would.
    ///
    /// A *closed* link is the same repair one step further out. The core never
    /// waits on a frontend, so a console that fell behind loses its attachment
    /// rather than slowing the session down — and one that then went on reading
    /// its last projection would be showing a session that had moved. It
    /// attaches again and re-reads.
    pub async fn reconcile_store(&mut self) -> bool {
        if self.store.is_closed() {
            let core = self.session.core.clone();
            return self.store.connect(&core, "tui").await.is_ok();
        }
        matches!(self.store.reconcile().await, Ok(true))
    }

    pub fn drain_all(&mut self) -> bool {
        // The core's stream first, and its rows with it: everything a run
        // reported is sequenced there, so reading the projection and building
        // the rows it changed are one step. The local channel carries what the
        // core does not know about — the transient tiers, the pinned panels, the
        // watch board — and is drained after.
        self.pump_store();
        let mut changed = self.drain_frames();
        changed |= self.drain_events();
        changed |= self.drain_asks();
        if changed {
            self.dirty = true;
            // The cheapest possible progress hook (D87): every stream delta,
            // tool event and ask that reaches the screen funnels through here,
            // so one assignment is the whole `stall` baseline.
            self.last_progress_tick = self.tick;
        }
        changed
    }

    /// Whether a busy turn has gone quiet past the `stall` threshold — the
    /// status row turns warning-coloured and stops glimmering.
    pub(crate) fn stalled(&self) -> bool {
        self.conv.busy && self.motion.stall(self.tick, self.last_progress_tick)
    }

    /// Whether the last turn's completion row is still inside its `settle`
    /// blink. Also what holds that message out of scrollback for the window.
    pub(crate) fn settling(&self) -> bool {
        self.conv
            .settle_at
            .is_some_and(|at| self.motion.settle(self.tick, at))
    }

    /// Deliver one event to the conversation it names.
    ///
    /// The addressed store is taken *out* of the console for the length of the
    /// handler. That is what lets one handler serve every conversation: the
    /// body needs `&mut self` for the markdown pipeline, the clock and the
    /// notifier, and it needs the transcript at the same time — with the store
    /// detached the two borrows are disjoint, and the code says which
    /// conversation it is writing into instead of assuming main.
    ///
    /// A handful of console reactions cannot run in that window because they
    /// read `Chat::conv` themselves — they start turns and drain the queue —
    /// so they run once the store is back ([`Follow`]).
    fn route(&mut self, addressed: crate::ui::Addressed) -> bool {
        let crate::ui::Addressed { to, event } = addressed;
        let Some(event) = self.console_event(event) else {
            return true;
        };
        let on_screen = to == self.active;
        let mut conv = self.detach(&to);
        let follow = self.handle(&to, &mut conv, event);
        self.attach(&to, conv);
        if follow.settle_asks {
            self.cancel_asks(true);
        }
        if follow.wake {
            self.intend(crate::tui::intent::Intent::Wake);
        }
        if let Some((label, reason)) = follow.alert {
            self.push_agent_alert(&label, reason.as_deref());
        }
        if let Some(label) = follow.notice {
            self.push_agent_notice(&label);
        }
        on_screen
    }

    /// Take `key`'s store out of the console, opening it if this is the first
    /// the console has heard of the conversation.
    ///
    /// First sight is an *event*, not a page opening: an instance streams from
    /// the moment it is spawned, and a page opened later shows what it did
    /// meanwhile because the store was there to receive it.
    ///
    /// **And the store opens blank, never walked** (D135). The cold-start walk
    /// reads the registry's history *now*, while the event in hand describes
    /// something that happened *then*: a run whose `finish` beat this drain is
    /// in that history already, and the deltas behind this event would replay
    /// it into a store nothing rebuilds — the doubled turn two of D134's
    /// reviewers found. Every instance is inserted with an empty history
    /// (`tool::agent`, `team::spawn`), so a store that opens on the first event
    /// misses nothing by starting empty: the walk's whole job is the
    /// conversation the console has *never* heard from, and that one is opened
    /// by [`Chat::claim_conversation`], which drains the channel first and so
    /// knows there is nothing left to replay.
    fn detach(&mut self, key: &crate::ui::ConvKey) -> crate::tui::conversation::Conversation {
        if *key == self.active {
            let usage = self.conv.context_usage;
            return std::mem::replace(
                &mut self.conv,
                crate::tui::conversation::Conversation::new(usage),
            );
        }
        match self.parked.remove(key) {
            Some(conv) => conv,
            None => self.blank_conversation(),
        }
    }

    /// A store with nothing in it but the model's window — what a conversation
    /// starts life with when the console has nothing to fill it from. The
    /// window is the model's and the same for everyone; what is *used* is per
    /// conversation and starts at nothing.
    pub(super) fn blank_conversation(&self) -> crate::tui::conversation::Conversation {
        let usage = self.conv.context_usage;
        crate::tui::conversation::Conversation::new(crate::context_usage::ContextUsage::new(
            0,
            usage.window,
            usage.trigger,
        ))
    }

    fn attach(&mut self, key: &crate::ui::ConvKey, conv: crate::tui::conversation::Conversation) {
        if *key == self.active {
            self.conv = conv;
        } else {
            self.parked.insert(key.clone(), conv);
        }
    }

    /// Main's store, wherever it is right now. The console's own machinery —
    /// its queue, its turn, the rows a background run hangs on it — is always
    /// about this one, even while the screen is on somebody else's page.
    pub(crate) fn main_conv(&mut self) -> &mut crate::tui::conversation::Conversation {
        if self.active.is_main() {
            return &mut self.conv;
        }
        let usage = self.conv.context_usage;
        self.parked
            .entry(crate::ui::ConvKey::Main)
            .or_insert_with(|| crate::tui::conversation::Conversation::new(usage))
    }

    fn handle(
        &mut self,
        to: &crate::ui::ConvKey,
        conv: &mut crate::tui::conversation::Conversation,
        event: UiEvent,
    ) -> Follow {
        let mut follow = Follow::default();
        match event {
            // The user's own line, as the core recorded it. Main's row, wherever
            // the screen is: a background run finishing wakes a turn while the
            // reader is standing on somebody else's page, and its rows belong in
            // the transcript that asked for them.
            UiEvent::Submitted(text) => {
                conv.messages.push(UiMessage {
                    speaker: None,
                    role: Role::User,
                    text,
                    at: crate::channels::now_unix(),
                    activities: Vec::new(),
                    insert_points: Vec::new(),
                    groups: Vec::new(),
                    group_of: Vec::new(),
                });
            }
            UiEvent::TurnStart => {
                // Watching the turn happen *is* reading the record.
                conv.history_read = true;
                conv.thinking_buf.clear();
                conv.thinking_seg_open = false;
                conv.pending_tools_clear();
                if to.is_main() {
                    // A new turn resets the error state (AC-03): page-level error rows vanish with the new turn
                    // (full-screen Full is already dismissed in error_screen_key; this is a fallback).
                    self.last_error = None;
                    // No command of the previous turn may keep painting under a row of
                    // this one (an interrupt drops the tool future without a ToolDone).
                    self.bash_tail = None;
                    self.interrupt_at = None;
                    // A fresh turn clears interrupt suppression — without this,
                    // one interrupt followed by only `!` commands kept
                    // background wake-ups suppressed for the rest of the session.
                    conv.interrupted = false;
                }
                let now = std::time::Instant::now();
                conv.turn_started = Some(now);
                conv.output_tokens = 0;
                conv.output_round_tokens = 0;
                conv.settle_at = None;
                conv.token_rate.start(now);
                if to.is_main() {
                    // The meter eases *one* status row toward *one* number (D87),
                    // and the title says the console is busy. Both are main's, and
                    // borrowing them for a background run would put somebody else's
                    // work under the console's name (D132's rule, kept).
                    self.token_meter.reset(0, self.tick);
                    self.notify
                        .set_title(Title::Busy(self.motion.title_glyph(self.tick)));
                }
                conv.messages.push(UiMessage {
                    speaker: None,
                    role: Role::Assistant,
                    text: String::new(),
                    at: crate::channels::now_unix(),
                    activities: Vec::new(),
                    insert_points: Vec::new(),
                    groups: Vec::new(),
                    group_of: Vec::new(),
                });
                conv.stream_msg = Some(conv.messages.len() - 1);
                // One verb per turn (D87): sampled here and reused by every
                // reasoning segment the turn opens.
                conv.turn_verb = thinking_stage(conv.messages.len());
                conv.stream_attempt_checkpoint = conv
                    .stream_msg
                    .and_then(|index| conv.messages.get(index).cloned());
                conv.continuation_msg = None;
                conv.busy = true;
                conv.turn_start_tick = self.tick;
                // Placeholder thinking: when the endpoint delays deltas (DeepSeek often by tens of seconds),
                // the running row is visible immediately.
                let mut hint = Activity::new(ActivityKind::Thinking(Thinking {
                    state: ThinkingState::Running,
                    duration_ms: 0,
                    stage: conv.turn_verb,
                    done_verb: Some(thinking_done_verb()),
                    start_tick: self.tick,
                    segments: 1,
                    timed: true,
                }));
                hint.expand_hint = Some(crate::tui::activities::EXPAND_HINT.to_string());
                if let Some(i) = conv.stream_msg {
                    conv.messages[i].activities.push(hint);
                    conv.messages[i].insert_points.push(0);
                    conv.messages[i].group_of.push(None);
                }
            }
            UiEvent::StreamRetry => {
                if let Some(index) = conv.stream_msg {
                    if let Some(checkpoint) = conv.stream_attempt_checkpoint.clone() {
                        conv.messages[index] = checkpoint;
                    }
                    let text_len = conv.messages[index].text.chars().count();
                    let mut hint = Activity::new(ActivityKind::Thinking(Thinking {
                        state: ThinkingState::Running,
                        duration_ms: 0,
                        stage: conv.turn_verb,
                        done_verb: Some(thinking_done_verb()),
                        start_tick: self.tick,
                        segments: 1,
                        timed: true,
                    }));
                    hint.expand_hint = Some(crate::tui::activities::EXPAND_HINT.to_string());
                    conv.messages[index].activities.push(hint);
                    conv.messages[index].insert_points.push(text_len);
                    conv.messages[index].group_of.push(None);
                }
                conv.thinking_buf.clear();
                conv.thinking_seg_open = false;
                conv.pending_tools_clear();
                conv.output_round_tokens = 0;
                conv.token_rate.retry_round();
            }
            UiEvent::TextDelta(text) => {
                if let Some(i) = conv.stream_msg
                    && !text.is_empty()
                {
                    conv.messages[i].text.push_str(&text);
                    if let Some(g) = conv.messages[i].groups.last_mut() {
                        g.active = false;
                    }
                    // Text is a segment boundary: thinking after text opens a new block (no more aggregation),
                    // and the running thinking block closes with it (same closing semantics as ToolStart).
                    conv.thinking_buf.clear();
                    conv.thinking_seg_open = false;
                    conv.close_running_thinking(i, self.tick);
                }
            }
            UiEvent::ThinkingDelta(thinking) => {
                if let Some(i) = conv.stream_msg {
                    let last_is_running_thinking =
                        conv.messages[i].activities.last().is_some_and(|a| {
                            matches!(&a.kind, ActivityKind::Thinking(t)
                                if t.state == ThinkingState::Running)
                        });
                    if last_is_running_thinking {
                        conv.thinking_buf.push_str(&thinking);
                        let buf = conv.thinking_buf.clone();
                        let content = self.render_thinking(&buf);
                        if let Some(hint) = conv.messages[i]
                            .activities
                            .iter_mut()
                            .rev()
                            .find(|a| matches!(a.kind, ActivityKind::Thinking(_)))
                        {
                            hint.set_content(content);
                        }
                    } else {
                        let dup = thinking == conv.thinking_buf
                            || conv.messages[i]
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
                            return follow;
                        }
                        // Aggregation: when text has not interrupted (thinking_buf still holds this stage's text),
                        // new reasoning merges into the last thinking block. Same-segment continuation (segment open)
                        // appends directly; a new segment (after a tool/text) is separated by a blank line and counted.
                        if !conv.thinking_buf.is_empty() {
                            let was_open = conv.thinking_seg_open;
                            if was_open {
                                conv.thinking_buf.push_str(&thinking);
                            } else {
                                conv.thinking_buf.push_str("\n\n");
                                conv.thinking_buf.push_str(&thinking);
                            }
                            conv.thinking_seg_open = true;
                            let buf = conv.thinking_buf.clone();
                            let content = self.render_thinking(&buf);
                            let merged = conv.messages[i]
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
                                        .saturating_mul(crate::tui::motion::TICK_MS);
                                }
                                hint.set_content(content);
                            }
                            return follow;
                        }
                        conv.thinking_buf = thinking.clone();
                        conv.messages[i].activities.retain(|a| {
                            !(matches!(a.kind, ActivityKind::Thinking(_)) && a.content.is_empty())
                        });
                        let buf = conv.thinking_buf.clone();
                        let content = self.render_thinking(&buf);
                        let mut hint = Activity::new(ActivityKind::Thinking(Thinking {
                            state: ThinkingState::Running,
                            duration_ms: self.tick.saturating_sub(conv.turn_start_tick)
                                * crate::tui::motion::TICK_MS,
                            stage: conv.turn_verb,
                            done_verb: Some(thinking_done_verb()),
                            start_tick: self.tick,
                            segments: 1,
                            timed: true,
                        }));
                        hint.set_content(content);
                        hint.expand_hint = Some(crate::tui::activities::EXPAND_HINT.to_string());
                        conv.messages[i].activities.push(hint);
                        let text_len = conv.messages[i].text.chars().count();
                        conv.messages[i].insert_points.push(text_len);
                        conv.messages[i].group_of.push(None);
                    }
                }
            }
            UiEvent::ContextUsage(usage) => {
                conv.context_usage = usage;
            }
            UiEvent::OutputTokens {
                tokens,
                authoritative,
            } => {
                conv.output_tokens = conv
                    .output_tokens
                    .saturating_sub(conv.output_round_tokens)
                    .saturating_add(tokens);
                conv.output_round_tokens = tokens;
                // The end-of-round usage total is a correction, not freshly streamed
                // output: fed as a sample it divided the jump by the live window and
                // rendered as a one-frame spike of thousands of tok/s.
                if authoritative {
                    conv.token_rate
                        .correct_round(tokens, std::time::Instant::now());
                } else {
                    conv.token_rate
                        .observe_round(tokens, std::time::Instant::now());
                }
            }
            UiEvent::ToolStart { name } => {
                if is_hidden_tool(&name) {
                    return follow;
                }
                if let Some(i) = conv.stream_msg {
                    conv.close_running_thinking(i, self.tick);
                }
                // Tool start = reasoning segment boundary: subsequent deltas aggregate into a new segment.
                conv.thinking_seg_open = false;
                let name: &'static str = Box::leak(name.into_boxed_str());
                let mut hint = Activity::new(ActivityKind::Tool(ToolCall::running(name, "")));
                hint.expand_hint = Some(crate::tui::activities::EXPAND_HINT.to_string());
                if let Some(i) = conv.stream_msg {
                    let idx = conv.messages[i].activities.len();
                    let text_len = conv.messages[i].text.chars().count();
                    conv.messages[i].activities.push(hint);
                    conv.messages[i].insert_points.push(text_len);
                    conv.messages[i].group_of.push(None);
                    conv.pending_tools_push(idx);
                }
            }
            UiEvent::ToolReady {
                tool_call_id,
                name,
                input,
                standalone,
            } => {
                let Some(i) = conv.stream_msg else {
                    return follow;
                };
                if is_hidden_tool(&name) {
                    return follow;
                }
                let Some(idx) = conv.pending_tools_pop() else {
                    return follow;
                };
                if let ActivityKind::Tool(call) = &mut conv.messages[i].activities[idx].kind {
                    call.summary = crate::query::summarize_input(&name, &input);
                    call.id = Some(tool_call_id);
                }
                // `!` commands: standalone activities (output preview expanded directly), not part of collapse groups.
                if standalone {
                    return follow;
                }
                group_ready_tool(&mut conv.messages[i], idx, &name, &input);
            }
            UiEvent::WatchEvent {
                label,
                kind,
                status,
                detail,
                duration_ms,
                payload,
                signal,
                notifies_main,
                dispatch,
            } => {
                // The registry sweep goes first: it is what materializes the
                // conversation an event is about, so a DM's badge is observed
                // against a buffer that is already there rather than one this
                // event has to create.
                //
                // D95 teed the event into a lifecycle feed as well; D107
                // retired that feed with the directory column that was its only
                // reader, and what a run did is read where it happens — the
                // flow's own dispatch and completion rows (D106), the dialog,
                // and the instance's record.
                self.refresh_conversations();
                let found = conv.messages.iter_mut().find_map(|m| {
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
                    // D94: which message, if any, this event may hang a row on.
                    //
                    // A running turn owns its own tools. `Agent` renders no tool
                    // row of its own (`is_hidden_tool`), so this watch row *is*
                    // the row for the Task call the user just watched the model
                    // make — main content, and it stays.
                    //
                    // With no turn running, the old code walked back to the last
                    // assistant message and stapled the row there. That is the
                    // message bus writing into the user's conversation: a
                    // background hire finishing, a continuation run opening under
                    // a new label, an ack watchdog reporting a chase — none of
                    // them answers anything the user did, and all of them landed
                    // under a reply that had nothing to do with them. The signal
                    // survives elsewhere: the instance's row in the background
                    // dialog (D107), its unread badge there, and — since D106 —
                    // the flow's own dispatch and completion rows, which belong
                    // to the turn that made the call.
                    //
                    // Command and channel watches keep the old walk-back: a
                    // background shell command is main's own tool, and a
                    // channel row belongs to the conversation it names.
                    //
                    // A streaming turn only staples the agent runs it asked
                    // for (D114): a room post waking a member mid-turn used to
                    // hang that member's run under whatever main happened to be
                    // saying — a "Running 3 agents" tree about work this turn
                    // never dispatched. Those runs live in the tree and the
                    // dialog; the flow's whitelist is main's own dispatches.
                    let target = match conv.stream_msg {
                        Some(_) if kind == crate::watch::WatchKind::Agent && !dispatch => None,
                        Some(i) => Some(i),
                        None if kind == crate::watch::WatchKind::Agent => None,
                        None => conv
                            .messages
                            .iter()
                            .rposition(|m| m.role == Role::Assistant),
                    };
                    match target {
                        Some(target) => {
                            let mut hint = Activity::new(ActivityKind::Watch(WatchCall {
                                label: label.clone(),
                                kind,
                                status,
                                detail: detail.clone(),
                                duration_ms,
                                progress: Vec::new(),
                                run_stats: None,
                            }));
                            hint.expand_hint =
                                Some(crate::tui::activities::EXPAND_HINT.to_string());
                            let text_len = conv.messages[target].text.chars().count();
                            conv.messages[target].activities.push(hint);
                            conv.messages[target].insert_points.push(text_len);
                            conv.messages[target].group_of.push(None);
                        }
                        // No message to hang a row on and not an agent event:
                        // the pre-D94 contract returned here, and the terminal
                        // handling below has never run for this case.
                        None if kind != crate::watch::WatchKind::Agent => return follow,
                        // An agent event with no row is the routing above doing
                        // its job. It must still fall through: `submit_auto` is
                        // how the completion reaches the *model*, and D94 changes
                        // only what the user sees.
                        None => {}
                    }
                }
                let terminal = matches!(
                    status,
                    WatchState::Done | WatchState::Failed | WatchState::Cancelled
                );
                if terminal || signal.is_some() {
                    if let Some(sig) = &signal
                        && let Some(hint) = conv.messages.iter_mut().find_map(|m| {
                            m.activities.iter_mut().find(
                                |a| matches!(&a.kind, ActivityKind::Watch(w) if w.label == *label),
                            )
                        })
                        && let ActivityKind::Watch(w) = &mut hint.kind
                    {
                        w.detail = Some(sig.clone());
                    }
                    // After the user interrupted a turn, never auto-run again (wait for an explicit submit).
                    //
                    // D98: and only when this run's end is actually addressed to
                    // the main agent. A run the user started in an agent's DM
                    // registers with `notify_owner: false`, so it enqueues
                    // nothing here — waking to digest an empty queue is how the
                    // console got loud in the first place. The same question is
                    // already asked at TurnEnd; asking it here too makes the two
                    // wake paths say one thing.
                    if !conv.interrupted && self.session.watch.has_wake_notifications(None) {
                        follow.wake = true;
                    }
                }
                // The two lines an agent's life writes into this flow.
                //
                // A run that **failed**, named, with its reason (D98): bad news
                // must not depend on the main agent choosing to narrate it,
                // because the turn that would have narrated it may never run.
                //
                // A run that **finished** and reported itself to main (D106):
                // one dim line for the task notification now sitting in main's
                // context, which is CC's `UserAgentNotificationMessage`. It is
                // gated on the notification actually being main's — a run the
                // user started inside an agent's own conversation registers with
                // `notify_owner: false` and tells nobody — on `Done`, because
                // a failure already has its line above and a cancellation is
                // something the user just did — and, since D114, on the run
                // being a dispatch: a delivery-triggered run still notifies
                // main's *context* exactly as before, but the flow says
                // nothing, because the user never asked for that run. The
                // `⚠` alert stays unconditional: bad news is whitelisted.
                if kind == crate::watch::WatchKind::Agent {
                    match status {
                        WatchState::Failed => follow.alert = Some((label.clone(), detail.clone())),
                        WatchState::Done if notifies_main && dispatch => {
                            follow.notice = Some(label.clone())
                        }
                        _ => {}
                    }
                }
            }
            UiEvent::RoundEnd => {
                conv.output_round_tokens = 0;
                conv.token_rate.finish_round();
                if let Some(i) = conv.stream_msg {
                    conv.stream_attempt_checkpoint = conv.messages.get(i).cloned();
                    // Collapse groups are bounded by text: model rounds do not split a group, nor does thinking —
                    // only text (TextDelta) and non-collapsible tools close the group.
                    // Warm the image cache a round early: by TurnEnd the message
                    // settles and flushes, and an image that only starts loading
                    // then would miss the flush (see `message_settled`).
                    let text = conv.messages[i].text.clone();
                    self.load_message_images(&text);
                }
            }
            UiEvent::ToolDone(done) => {
                // The finished call's own result row takes over from the tail. A
                // sample still in flight when the command exited would otherwise
                // paint output under a row that already reported it. The tail is
                // the console's one foreground command, so only main's calls
                // clear it — an instance running Bash used to blank it (D134).
                if done.name == "Bash" && to.is_main() {
                    self.bash_tail = None;
                }
                // A tool that handed the model a picture hands the user one too
                // (D97). The file is on disk under the name the tool used, so
                // the registry points at it and copies nothing.
                if done.status == crate::query::ToolCallStatus::Done
                    && let Some((path, bytes)) = crate::tool::read::image_result_path(&done.output)
                {
                    self.image_registry
                        .register_file(&path, crate::tui::buffer::now(), bytes);
                }
                let Some(i) = conv.stream_msg else {
                    return follow;
                };
                if let Some(diff_text) = &done.diff
                    && let Some(pos) = conv.messages[i].activities.iter().position(|h| {
                        matches!(&h.kind, ActivityKind::Tool(c)
                            if c.name == done.name.as_str() && c.status == ToolStatus::Running)
                    })
                {
                    let diff = Diff::parse_unified(diff_text);
                    // `layout_activity` prefixes RESULT_INDENT to every content
                    // row, so that is the width the diff itself has to fit in.
                    let content = diff_lines(&diff, &self.theme, self.diff_width());
                    let mut hint = Activity::new(ActivityKind::Diff(diff));
                    hint.expand_hint = Some(crate::tui::activities::EXPAND_HINT.to_string());
                    hint.set_content(content);
                    conv.messages[i].activities[pos] = hint;
                    return follow;
                }
                let group_of = conv.messages[i].group_of.clone();
                for (hint_idx, hint) in conv.messages[i].activities.iter_mut().enumerate() {
                    // The call the protocol named, or — for a row the protocol
                    // never named — the first running one wearing this tool's
                    // name (D134).
                    if let ActivityKind::Tool(call) = &mut hint.kind
                        && call.status == ToolStatus::Running
                        && match &call.id {
                            Some(id) => id == &done.tool_call_id,
                            None => call.name == done.name.as_str(),
                        }
                    {
                        call.status = match done.status {
                            crate::query::ToolCallStatus::Done => ToolStatus::Done,
                            crate::query::ToolCallStatus::Error => ToolStatus::Error,
                            crate::query::ToolCallStatus::Interrupted => ToolStatus::Interrupted,
                        };
                        call.summary = done.summary.clone();
                        call.duration_ms = done.duration_ms;
                        if call.status == ToolStatus::Interrupted {
                            // Nothing to summarize or expand: the call was stopped before
                            // it produced a result, and the status line says so.
                            break;
                        }
                        let in_group = group_of.get(hint_idx).copied().flatten().is_some();
                        if in_group {
                            call.result_summary = result_summary(&done.name, &done.output);
                        } else if done.name == "Skill" {
                            call.result_summary = skill_result_summary(&done.output);
                        }
                        // Every finished call keeps its output, grouped or not: inside a fold the
                        // row summary (`Read 173 lines`) is all that survives, so dropping the
                        // content there makes the output unreachable for the rest of the session.
                        hint.set_content(result_content(&done.name, &done.output));
                        // Standalone Bash (`!` commands) is expanded by default
                        // (BashModeProgress shows the output directly); a grouped call stays
                        // folded until the group opens.
                        if !in_group && done.name == "Bash" && !hint.expanded {
                            hint.expanded = true;
                        }
                        break;
                    }
                }
            }
            UiEvent::TurnEnd => {
                conv.busy = false;
                if to.is_main() {
                    self.bash_tail = None;
                }
                // The `settle` blink starts here (D87): the completion row keeps
                // the accent for one 120ms window, and the message it belongs to
                // stays live for exactly that long, so the row freezes into
                // scrollback at rest and write-once is never broken.
                conv.settle_at = Some(self.tick);
                // A turn short enough to have been watched needs no
                // notification; a long one is exactly what the user walked away
                // from (D79). Read before the start time is cleared.
                if to.is_main() {
                    if conv
                        .turn_started
                        .is_some_and(|at| at.elapsed() >= crate::tui::notify::LONG_TURN)
                    {
                        self.notify.attention(Attention::TurnComplete);
                    }
                    self.notify_idle();
                    follow.settle_asks = true;
                }
                conv.turn_started = None;
                conv.output_tokens = 0;
                conv.output_round_tokens = 0;
                conv.token_rate.stop();
                conv.thinking_seg_open = false;
                conv.drop_empty_stream_message();
                // AskUserQuestion answers are ordinary user messages (in the message flow,
                // settled/flushed with it) — nothing to clean at turn end, they persist with the session.
                // After a user interruption, background-task completion must not auto-start a new turn;
                // with queued messages, the user's message goes first (submitted together below).
                //
                // Mail (room relays, direct messages) is deliberately not part of
                // this condition since D98: it wakes through the digest debounce
                // on the tick, so a burst costs one turn rather than one per
                // message, and both wake paths cannot disagree about when.
                if to.is_main()
                    && self.session.watch.has_wake_notifications(None)
                    && !conv.interrupted
                    && self.main_queue().is_empty()
                {
                    follow.wake = true;
                }
                if let Some(i) = conv.stream_msg {
                    // The reply's send time is when it landed, not when the turn
                    // opened — the same clock the workspace DM stamps carry.
                    conv.messages[i].at = crate::channels::now_unix();
                    // @main's unread (D99): main just spoke, and the accounting
                    // store carries the count D104's pills read. Prose only — a
                    // turn that said nothing has nothing to come back for, and
                    // `observe` zeroes the count outright while @main is the
                    // active conversation.
                    if to.is_main() && !conv.messages[i].text.trim().is_empty() {
                        self.buffers.note_console(false, self.tick);
                    }
                    if let Some(g) = conv.messages[i].groups.last_mut() {
                        g.active = false;
                    }
                    // Remove synchronously: the empty placeholder thinking and its insert point.
                    let mut keep = Vec::new();
                    for (idx, a) in conv.messages[i].activities.iter().enumerate() {
                        if matches!(a.kind, ActivityKind::Thinking(_)) && a.content.is_empty() {
                            continue;
                        }
                        keep.push(idx);
                    }
                    if keep.len() != conv.messages[i].activities.len() {
                        let old_to_new: HashMap<usize, usize> = keep
                            .iter()
                            .enumerate()
                            .map(|(new, old)| (*old, new))
                            .collect();
                        for g in &mut conv.messages[i].groups {
                            g.activities = g
                                .activities
                                .iter()
                                .filter_map(|a| old_to_new.get(a).copied())
                                .collect();
                        }
                        conv.messages[i].activities = keep
                            .iter()
                            .map(|&k| conv.messages[i].activities[k].clone())
                            .collect();
                        conv.messages[i].insert_points = keep
                            .iter()
                            .map(|&k| conv.messages[i].insert_points[k])
                            .collect();
                        conv.messages[i].group_of =
                            keep.iter().map(|&k| conv.messages[i].group_of[k]).collect();
                    }
                    for hint in &mut conv.messages[i].activities {
                        match &mut hint.kind {
                            ActivityKind::Thinking(t) if t.state == ThinkingState::Running => {
                                t.state = ThinkingState::Done;
                                t.duration_ms = self
                                    .tick
                                    .saturating_sub(t.start_tick)
                                    .saturating_mul(crate::tui::motion::TICK_MS);
                                hint.expanded = false;
                            }
                            // A call still running when the turn ends never got a
                            // `ToolDone`: the run was aborted under it. Left as it
                            // was it reads `Running…` for the rest of the session
                            // and pins the flush cursor with it, because
                            // `message_static_settled` refuses a running activity
                            // and settlement is prefix-monotone — an unbounded
                            // redrawable tail on that page. Before D134 a stop was
                            // survivable because the page re-read the committed
                            // history; the live store is authoritative now and has
                            // to correct itself.
                            ActivityKind::Tool(call)
                                if call.status == crate::tui::activities::ToolStatus::Running =>
                            {
                                call.status = crate::tui::activities::ToolStatus::Interrupted;
                            }
                            _ => {}
                        }
                    }
                    // Text is settled → asynchronously load its images (reply with ImageReady when done).
                    let text = conv.messages[i].text.clone();
                    self.load_message_images(&text);
                }
                conv.stream_msg = None;
                conv.stream_attempt_checkpoint = None;
                // Draining the queue is not here any more, and not the console's
                // at all: main's turn ending is what lets the next entry start,
                // and the turn registry is where that ending is known
                // (`Controller::drain_main`, D154). Two drains would take the
                // same entry twice.
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
                    conv.busy = false;
                    conv.drop_empty_stream_message();
                    conv.stream_msg = None;
                    conv.stream_attempt_checkpoint = None;
                }
                // A flow-level failure ends the turn on a screen the user has to
                // come back to; a page-level one is a hint beside a session that
                // carries on, and carries no notification (D79).
                if level == crate::error::ErrorLevel::Full {
                    self.notify.attention(Attention::TurnFailed);
                    self.notify_idle();
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
            UiEvent::Mail { from, text } => {
                // A message that has been *delivered*. It is filed by the same
                // builder an absorbed prompt is filed by, so a page cannot draw
                // the two differently — and it is spliced by `absorb_inbound`,
                // so a question still lands above the answer it caused (D134a).
                let at = crate::channels::now_unix();
                let arrived = vec![crate::tui::conv::counterpart_message(&from, at, text)];
                conv.absorb_inbound(arrived, self.tick);
                // A delivered message accounts for the receiver's first
                // user-role text, so the run's repeat of it is not the task the
                // instance was spawned with — only a hire's prompt is, and that
                // one never comes through the inbox.
                conv.intake_seen = true;
                self.dirty = true;
            }
            // Console-wide events never reach here: `console_event` answers them
            // and only hands on what a transcript owns.
            other => debug_assert!(false, "unrouted console event: {other:?}"),
        }
        follow
    }

    /// Events that belong to the console rather than to any one transcript:
    /// the image cache, the transient tiers, the pinned panels, and the queue
    /// the running turn just took from. Returns the event again when it is not
    /// one of them, so the caller can hand it to the conversation it names.
    fn console_event(&mut self, event: UiEvent) -> Option<UiEvent> {
        match event {
            UiEvent::ModelsLoaded {
                provider,
                models,
                failed,
            } => self.apply_models_loaded(provider, models, failed),
            UiEvent::ImageReady { url, meta } => {
                self.images_pending.remove(&url);
                match meta {
                    Some(meta) => {
                        self.images_failed.remove(&url);
                        // A picture that placed on screen is a picture the user
                        // can ask to see properly (D97). This is the tee for
                        // everything that arrives as a markdown image — an
                        // agent's chart, a tool's output, a URL in the model's
                        // prose — and it fires here rather than at load time so
                        // a failed fetch never lands in the list.
                        self.image_registry.register_bytes(
                            &url,
                            crate::tui::buffer::now(),
                            meta.bytes.clone(),
                        );
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
            UiEvent::BashTail(tail) => {
                // Only worth keeping while there is a row to hang it under; the
                // renderer decides that, and drops it the moment the call is done.
                self.bash_tail = Some(tail);
            }
            UiEvent::Steered { items } => {
                self.absorb_steered(&items);
            }
            UiEvent::Interrupted(marker) => {
                self.push_interrupt_marker(&marker);
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
            UiEvent::RewindDone(message) => {
                self.push_user_line(message);
                self.refresh_context_usage_from_transcript();
                self.dirty = true;
            }
            UiEvent::PinPanel { id, lines } => {
                self.pin_panel(&id, lines);
            }
            UiEvent::Unpin { id } => {
                self.unpin_panel(&id);
            }
            other => return Some(other),
        }
        None
    }

    #[cfg(test)]
    fn apply_turn_start(&mut self) {
        self.apply_event(UiEvent::TurnStart);
    }

    /// One event into the conversation on screen — the shape a test writes when
    /// it is describing what the reader sees.
    #[cfg(test)]
    pub(crate) fn apply_event(&mut self, event: UiEvent) {
        let to = self.active.clone();
        self.route(crate::ui::Addressed { to, event });
    }

    /// One event into a named conversation, for the tests that are about
    /// somebody else's turn arriving while the screen is elsewhere.
    #[cfg(test)]
    pub(crate) fn apply_event_to(&mut self, to: crate::ui::ConvKey, event: UiEvent) {
        self.route(crate::ui::Addressed { to, event });
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
                events.send(UiEvent::ImageReady { url, meta });
            });
        }
    }

    /// Thinking content renders with markdown streaming (code blocks/lists update as the stream flows).
    /// Re-renders with the full text each time (thinking deltas are small).
    pub(crate) fn render_thinking(&mut self, text: &str) -> Vec<Line> {
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
        // An image row is its own click target and needs no range: the row
        // already carries the URL its picture was loaded from, and a bubble
        // carries the `#[image N]` marker. Checked before the ranges, because
        // an image inside a tool's output would otherwise be swallowed by the
        // enclosing collapse group.
        let hit = self
            .doc
            .rows
            .get(doc_row)
            .and_then(|row| self.image_at_row(row));
        if let Some(id) = hit {
            self.open_image(id);
            return true;
        }
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
                let Some(msg) = self.conv.messages.get_mut(*message) else {
                    return false;
                };
                let Some(g) = msg.groups.get_mut(*group) else {
                    return false;
                };
                g.expanded = !g.expanded;
                // Members follow the group. Every row of an open group is wrapped in the
                // group's own click target (the enclosing wrapper wins over the annotations
                // inside it), so a member row cannot be opened on its own with the mouse:
                // either the group carries its members' state or their output stays behind
                // a keystroke.
                let expanded = g.expanded;
                let members = g.activities.clone();
                for idx in members {
                    if let Some(act) = msg.activities.get_mut(idx) {
                        act.expanded = expanded;
                    }
                }
                self.auto_scroll = false;
                self.dirty = true;
                true
            }
            ClickTarget::Activity { message, path } => {
                let Some(msg) = self.conv.messages.get_mut(*message) else {
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

    /// Open every fold of the most recent message.
    ///
    /// Test-facing only: it is the expanded *presentation* many render tests
    /// assert on, and reaching it through the production surfaces would mean
    /// driving a mouse click ([`Chat::doc_click`]) or an alternate-screen pager
    /// ([`crate::tui::transcript`]) for a question about rows. The global
    /// ctrl+o toggle this replaced is gone: in-place expansion could never work
    /// in inline mode, where the rows it would rewrite already belong to the
    /// terminal.
    #[cfg(test)]
    pub fn expand_all_folds(&mut self) -> bool {
        let Some(i) = self.conv.messages.len().checked_sub(1) else {
            return false;
        };
        for act in &mut self.conv.messages[i].activities {
            act.expanded = true;
        }
        for group in &mut self.conv.messages[i].groups {
            group.expanded = true;
        }
        self.auto_scroll = false;
        self.dirty = true;
        true
    }

    /// The one input path (D135): the same steps wherever the screen is.
    ///
    /// v6 gave an agent's page a composer of its own, which sent the whole
    /// line as prose and never looked at it — so `/`, `!` and the `@name`
    /// grammar were dead on every page but main's, while the placeholder went
    /// on advertising them. The console's commands are the *console's*: a
    /// terminal command and shell mode do not stop existing because of which
    /// conversation you are reading. What is genuinely per-conversation is the
    /// last step alone — prose goes to whoever the screen is pointed at, which
    /// is the one difference main has, that it talks to the user by default.
    pub fn submit(&mut self) {
        let raw = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.undo.clear();
        self.last_edit = None;
        if raw.trim().is_empty() {
            self.set_input(raw);
            return;
        }
        // Terminal-only shorthand resolves before the core sees the line: a paste
        // placeholder and an image path are this surface's own, and the core is
        // handed text and assets (spec "One submission path").
        let text = self.expand_pastes(&raw);
        let text = self.expand_image_paths(&text);
        // The line is recorded rather than sent from here: a key handler cannot
        // wait on the actor, and the answer is folded in before the frame that
        // would show it (D154).
        let mode = if self.bash_mode {
            crate::app::command::ComposerMode::Shell
        } else {
            crate::app::command::ComposerMode::Normal
        };
        let carries_attachments = !self.resolve_images(&text).is_empty();
        self.intend(crate::tui::intent::Intent::Submit {
            raw,
            text,
            mode,
            on: self.active.clone(),
            carries_attachments,
        });
    }

    /// What the console does about what the core did.
    ///
    /// Nothing here performs the submission — by the time this runs the turn is
    /// open, the message is in its inbox or the entry is on the queue (D154).
    /// What is left is the console's own half: its input history, its feedback
    /// tiers, and the dropdown. The rows the submission produced are drawn from
    /// the core's own stream like every other row on the page.
    pub(crate) fn drew(
        &mut self,
        performed: crate::app::submit::Performed,
        raw: String,
        text: String,
    ) {
        use crate::app::submit::Performed;
        match performed {
            Performed::Nothing => self.set_input(raw),
            // The whole line goes into history, envelope included: ↑ brings back
            // what was typed, not what was delivered.
            Performed::Delivered {
                target,
                addressed: true,
                ..
            } => {
                self.record_history(&raw);
                self.refresh_conversations();
                // The receipt is transient and lives on the info tier: nothing
                // was said to the model, so nothing belongs in main's history.
                self.push_slash_info(format!("Sent to {}", target.label()));
                self.update_slash_suggestions();
            }
            // A refusal says what did not happen, on the same tier and never as
            // a receipt — a receipt claims something was delivered.
            Performed::Undelivered {
                addressed: true,
                why,
                ..
            } => {
                self.record_history(&raw);
                self.push_slash_info(why);
                self.update_slash_suggestions();
            }
            // The page's own prose. There is no receipt, because what a receipt
            // would announce is drawn on this very screen the moment the frame
            // redraws (D105).
            Performed::Delivered { .. } => {
                self.record_history(&text);
                self.refresh_conversations();
                self.update_slash_suggestions();
                self.dirty = true;
            }
            // A refusal is news and takes the warning tier.
            Performed::Undelivered { why, .. } => {
                self.record_history(&text);
                self.push_warning(why);
                self.update_slash_suggestions();
                self.dirty = true;
            }
            Performed::Queued(_) => {
                // The queue rows are main's page. Somewhere else, a line that
                // silently joined a queue nobody can see is a keystroke that did
                // nothing, so it says so on the tier the page does show.
                if !self.active.is_main() {
                    self.push_slash_info("queued behind main's turn".to_string());
                }
                self.update_slash_suggestions();
                self.dirty = true;
            }
            Performed::Shell { command, .. } => {
                self.record_history(&text);
                self.bash_history.push(command);
                self.dirty = true;
            }
            Performed::Command { line, .. } => {
                // A command the core let through while a turn runs is a
                // side-channel dispatch: it must not reset `busy`, it leaves no
                // history entry, and the dropdown has nothing to do with it.
                if self.main_conv().busy {
                    self.run_slash(&line);
                    self.update_slash_suggestions();
                    return;
                }
                self.record_history(&text);
                self.run_completed_slash(&line);
            }
            Performed::Turn { .. } => {
                self.record_history(&text);
                self.last_prompt = text;
                self.dirty = true;
            }
            // Only a session assembled without an engine answers this, and the
            // console's is never one. Saying so is better than a keystroke that
            // did nothing.
            Performed::Unavailable => {
                self.set_input(raw);
                self.push_warning("this session cannot run a turn".to_string());
            }
        }
    }

    /// Prose the console submits on the user's behalf: the retry an error screen
    /// offers, and the marker a skill invocation becomes.
    ///
    /// It goes through the same door a typed line goes through, as
    /// [`crate::app::command::Submission::SendProse`] — which parses none of the
    /// composer's forms, so a skill whose marker opens with `/` is prose and not
    /// a command.
    pub(crate) fn resubmit(&mut self, text: String) {
        self.intend(crate::tui::intent::Intent::Resubmit(text));
    }

    /// Hand this line to the core and hear what it did with it, for a test whose
    /// subject is the disposition rather than the rows it produced.
    ///
    /// Whether main is busy is not stated here: the turn registry is the core's,
    /// and a caller that could state it could also state it wrongly.
    #[cfg(test)]
    pub(crate) fn route_submission(&mut self, text: &str) -> crate::app::submit::Performed {
        let mode = if self.bash_mode {
            crate::app::command::ComposerMode::Shell
        } else {
            crate::app::command::ComposerMode::Normal
        };
        let carries_attachments = !self.resolve_images(text).is_empty();
        self.session
            .submit
            .submit(crate::app::submit::SubmitRequest {
                conversation: self.active.clone(),
                input: crate::app::command::Submission::Composer {
                    mode,
                    text: text.to_string(),
                    attachments: Vec::new(),
                },
                carries_attachments,
            })
            .now()
    }

    /// Put main into a running turn the way a run does.
    ///
    /// `conv.busy = true` used to be enough, because the console's flag was also
    /// what the core was told. It no longer is: the turn registry answers whether
    /// main is busy, so a test that wants a busy main opens a turn there. The
    /// console's own flag follows it — that half is still the console's until it
    /// reads the store for it.
    #[cfg(test)]
    pub(crate) fn start_test_turn(&mut self) -> Option<crate::app::ids::TurnId> {
        let turn = self.open_core_turn(crate::app::snapshot::TurnOrigin::User);
        // Folded in before anything else looks: a real console reads the turn's
        // own start off the stream, and a test that skipped it would find the
        // event later, in the middle of whatever it was actually asserting.
        self.settle_store();
        self.main_conv().busy = true;
        turn
    }

    /// Open a turn on a page the way its own producer does, so the events that
    /// follow have a turn to belong to.
    #[cfg(test)]
    pub(crate) fn start_test_turn_on(
        &mut self,
        key: &crate::ui::ConvKey,
    ) -> crate::app::ids::TurnId {
        let turn = self
            .session
            .turns
            .open(
                key.clone(),
                crate::app::snapshot::TurnOrigin::Peer,
                Vec::new(),
            )
            .now()
            .unwrap_or_else(|| panic!("{key:?} was idle"));
        self.settle_store();
        turn
    }

    /// What a run reports as it opens: the prose it was handed, read by the
    /// core's one walker and committed as the messages that entered the
    /// conversation. The console draws them from that, which is the whole point
    /// — a page cannot attribute the live half and the settled half differently
    /// if there is only one reader.
    #[cfg(test)]
    pub(crate) fn report_inbound(&mut self, turn: &crate::app::ids::TurnId, text: &str) {
        self.session.turns.report_event(
            turn.clone(),
            crate::engine::events::EngineEvent::Inbound(text.to_string()),
        );
        self.settle_store();
    }

    /// End the turn `start_test_turn` opened, and the console's flag with it.
    #[cfg(test)]
    pub(crate) fn end_test_turn(&mut self) {
        if let Some(turn) = self.session.turns.active(crate::ui::ConvKey::Main).now() {
            self.session
                .turns
                .close(turn, crate::app::snapshot::TurnStatus::Completed, None);
            // Closing is a report, not a question. Asking one afterwards is how
            // this side knows the report was read: both travel the actor's one
            // queue, in order.
            let _ = self.session.turns.active(crate::ui::ConvKey::Main).now();
        }
        self.main_conv().busy = false;
    }

    /// Run a slash command, letting the dropdown finish a partial name first.
    ///
    /// Enter with a partial prefix and suggestions showing applies the selection
    /// and runs it (handleEnter: with suggestions present, Enter = complete +
    /// execute). Only in the command-NAME phase: an argument dropdown lists
    /// values, and running the selected one as a command would dispatch
    /// `/deepseek-chat` when the user meant `/model deepseek-chat`.
    fn run_completed_slash(&mut self, line: &str) -> bool {
        if self.slash_arg_start.is_none()
            && !self.slash_suggestions.is_empty()
            && !self
                .slash_suggestions
                .iter()
                .any(|s| s.name == line.trim_end())
        {
            let selected = self.slash_suggestions.get(self.slash_selected).cloned();
            self.clear_slash_suggestions();
            if let Some(s) = selected
                && self.run_slash(&s.name)
            {
                return true;
            }
        }
        self.run_slash(line)
    }

    /// Put a prompt on screen without a run behind it, for a test whose subject
    /// is what the dialog does to the surface around it rather than what
    /// answering it does.
    #[cfg(test)]
    pub(crate) fn stub_ask(&mut self, request: PermissionRequest) {
        self.pending_ask = Some((crate::app::ids::InteractionId::new("int_stub"), request));
    }

    /// Open a real prompt in the core and put it on screen. The receiver is the
    /// verdict the run would have been given.
    #[cfg(test)]
    pub(crate) fn open_test_prompt(
        &mut self,
        prompt: crate::app::snapshot::InteractionPrompt,
    ) -> tokio::sync::oneshot::Receiver<crate::app::interaction::Verdict> {
        let verdict = self
            .session
            .interactions
            .open(crate::app::interaction::OpenPrompt {
                conversation: crate::ui::ConvKey::Main,
                turn: None,
                item: None,
                prompt,
            });
        // The console learns about a prompt the way every client does: from the
        // frame the core published. Let it finish saying so, then look.
        self.settle_store();
        self.drain_asks();
        verdict
    }

    /// AskUserQuestion's shape: the model's own options plus the free-text row.
    #[cfg(test)]
    pub(crate) fn open_test_question(
        &mut self,
        title: &str,
        question: &str,
        options: &[(&str, Option<&str>)],
    ) -> tokio::sync::oneshot::Receiver<crate::app::interaction::Verdict> {
        self.open_test_prompt(crate::app::snapshot::InteractionPrompt::Question {
            title: title.to_string(),
            question: question.to_string(),
            options: options
                .iter()
                .enumerate()
                .map(
                    |(index, (label, description))| crate::app::snapshot::QuestionOption {
                        id: index.to_string(),
                        label: (*label).to_string(),
                        description: description.map(str::to_string),
                    },
                )
                .collect(),
            allows_free_text: true,
        })
    }

    /// The permission gate's shape, with the session option offered only when a
    /// rule was derived for it.
    #[cfg(test)]
    pub(crate) fn open_test_permission(
        &mut self,
        tool: &str,
        reason: &str,
        command: Option<&str>,
        scope: Option<&str>,
    ) -> tokio::sync::oneshot::Receiver<crate::app::interaction::Verdict> {
        use crate::app::snapshot::PermissionDecisionKind;
        let mut decisions = vec![PermissionDecisionKind::AllowOnce];
        if scope.is_some() {
            decisions.push(PermissionDecisionKind::AllowSession);
        }
        decisions.push(PermissionDecisionKind::Deny);
        self.open_test_prompt(crate::app::snapshot::InteractionPrompt::Permission {
            title: format!("Allow running {tool}"),
            reason: Some(reason.to_string()),
            tool: crate::app::snapshot::ToolRequest {
                name: tool.to_string(),
                input: command
                    .map(|command| serde_json::json!({ "command": command }))
                    .unwrap_or(serde_json::Value::Null),
            },
            preview: command.map(
                |command| crate::app::snapshot::InteractionPreview::Command {
                    command: command.to_string(),
                },
            ),
            decisions,
            session_scope: scope.map(|label| crate::app::snapshot::SessionScope {
                id: crate::app::ids::ScopeId::new(""),
                label: label.to_string(),
            }),
            allows_feedback: true,
        })
    }

    /// Put a line straight on the console's queue, for a test that wants one
    /// there without a turn to queue it behind.
    ///
    /// Production reaches the queue through [`Chat::submit`], where the core
    /// decides that the line waits at all.
    #[cfg(test)]
    pub(crate) fn enqueue(&mut self, text: String, on: crate::ui::ConvKey) {
        let kind = if text.starts_with('/') {
            crate::app::queue::QueuedKind::Command
        } else {
            crate::app::queue::QueuedKind::Prose
        };
        let carries_attachments = !self.resolve_images(&text).is_empty();
        self.session
            .queue
            .enqueue(crate::app::queue::Enqueue {
                conversation: crate::ui::ConvKey::Main,
                origin: on,
                text,
                kind,
                attachments: Vec::new(),
                carries_attachments,
            })
            .now();
        self.dirty = true;
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
        // Everything below is one edit that the user did not type, so the
        // completion surfaces close rather than reopen on whatever character
        // the payload happens to end with (D86).
        self.pasting = true;
        self.paste_inner(text);
        self.pasting = false;
    }

    fn paste_inner(&mut self, text: &str) {
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
        self.register_image(&bytes, "clipboard")
    }

    /// Swaps placeholders back to their real content (at submit time).
    pub(crate) fn expand_pastes(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (token, body) in &self.pastes {
            out = out.replace(token.as_str(), body);
        }
        out
    }

    /// An image path in the input (a standalone path line, or a whole `![alt](path)` line) → read the file
    /// → compress and register → replace with the `#[image N]` placeholder. Unrecognized/unreadable lines stay as-is.
    pub(crate) fn expand_image_paths(&mut self, text: &str) -> String {
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
    ///
    /// The content registry is teed here too (D97): the wire copy is what the
    /// model sees, the registry copy is what the user can open. `source` names
    /// where the bytes came from, because a clipboard paste has no filename.
    fn register_image(&mut self, bytes: &[u8], source: &str) -> Option<usize> {
        let marker = self.session.attachments.register(bytes)?;
        let id =
            self.image_registry
                .register_bytes(source, crate::tui::buffer::now(), bytes.to_vec());
        self.image_registry.set_marker(id, marker);
        Some(marker)
    }

    /// Image file → register the attachment (read failure / non-image → None).
    fn register_image_file(&mut self, path: &std::path::Path) -> Option<usize> {
        let bytes = std::fs::read(path).ok()?;
        let marker = self.session.attachments.register(&bytes)?;
        // The file is already on disk under a name the user chose, so the
        // registry addresses it there instead of writing a second copy.
        let id = self
            .image_registry
            .register_file(path, crate::tui::buffer::now(), bytes.len());
        self.image_registry.set_marker(id, marker);
        Some(marker)
    }

    /// Open a registry image in the desktop's viewer.
    ///
    /// Never blocks: the file is written if it was only in memory, the viewer
    /// is spawned detached, and a failure is one info line — the tier for
    /// something the user explicitly asked for and did not get.
    pub(crate) fn open_image(&mut self, id: usize) {
        // The confirmation names the picture the way the user knows it, not
        // the temp path it happens to have been written to.
        let label = self
            .image_registry
            .get(id)
            .map(|e| e.source.clone())
            .unwrap_or_else(|| format!("image {id}"));
        let path = match self.image_registry.materialize(id) {
            Ok(path) => path,
            Err(e) => {
                self.push_slash_info(format!("could not open image: {e}"));
                return;
            }
        };
        let opened = match &self.image_opener {
            Some(program) => crate::tui::images::open_detached(program, &[], &path),
            None => {
                let (program, leading) = crate::tui::images::desktop_opener();
                crate::tui::images::open_detached(program, leading, &path)
            }
        };
        match opened {
            Ok(()) => self.push_slash_output(format!("opening {label}")),
            Err(e) => self.push_slash_info(format!("could not open image: {e}")),
        }
    }

    /// The registry entry a document row addresses, if it addresses one: the
    /// image block's own URL first, then the `#[image N]` marker a user bubble
    /// carries. Both are rows the user can see a picture in, so both are rows
    /// a click can open.
    pub(crate) fn image_at_row(&self, row: &Row) -> Option<usize> {
        if let Some(img) = &row.line.image
            && let Some(entry) = self.image_registry.by_source(&img.url)
        {
            return Some(entry.id);
        }
        let text = row.line.plain_text();
        crate::api::image::first_marker(&text)
            .and_then(|marker| self.image_registry.by_marker(marker))
            .map(|entry| entry.id)
    }

    /// Apply one action to the core, which is where the session's configuration
    /// lives (D154). The console renders the outcome itself; what this is for is
    /// making sure there is one thing to render it *from*.
    pub(crate) fn apply_to_core(&mut self, action: crate::app::command::Action) {
        self.intend(crate::tui::intent::Intent::Execute(Box::new(action)));
    }

    /// The permission mode in effect, read from the projection rather than kept.
    ///
    /// Shift+tab used to set a copy here while `/permission-mode` set the core's,
    /// which is two answers to one question. There is one now: the core's, as
    /// `config/read` publishes it. Before the first cut lands, the mode the
    /// session started in is the honest answer.
    pub fn permission_mode(&self) -> PermissionMode {
        match &self.store.view().config {
            Some(config) => console_permission_mode(config.permission_mode),
            None => self.session.permission_mode,
        }
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
            || self.images_menu.is_some()
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
        self.images_menu = None;
        self.dirty = true;
    }

    /// Clears the slash dropdown, its no-match flag and its argument phase
    /// together (single lifecycle).
    pub(crate) fn clear_slash_suggestions(&mut self) {
        self.slash_suggestions.clear();
        self.slash_no_match = false;
        self.slash_arg_start = None;
    }

    /// Slash command dispatch. Returns true = consumed.
    /// A slash command typed on the page that is up right now.
    pub(crate) fn run_slash(&mut self, line: &str) -> bool {
        let on = self.active.clone();
        self.run_slash_on(line, &on)
    }

    /// A slash command, with the page it was typed on carried explicitly.
    ///
    /// The two are the same call except when the command was queued: it drains
    /// at TurnEnd, and by then `active` answers a question about *now* rather
    /// than about where the user was when they typed (D135a).
    pub(crate) fn run_slash_on(&mut self, line: &str, on: &crate::ui::ConvKey) -> bool {
        // Any slash run closes the dropdown (Enter on a full input skips submit's clear-menu branch,
        // otherwise suggestion rows like `+ /model …` would linger below the input forever).
        self.clear_slash_suggestions();
        let arg = match line.split_once(char::is_whitespace) {
            Some((_, a)) => a.trim(),
            None => "",
        };
        // What the line *is* comes from the one command table (D146): its names,
        // its aliases, its argument grammar, and the skills this session has.
        // What each branch then *does* is still the console's, until B7 sends
        // these as `action/execute` frames instead.
        let skills: Vec<String> =
            crate::skills::load_skills(&self.session.home, &std::path::PathBuf::from(&self.cwd))
                .into_iter()
                .map(|skill| skill.name)
                .collect();
        let parsed = match crate::app::action::parse_in(line, &skills) {
            Ok(parsed) => parsed,
            Err(error) => {
                let code = match error {
                    crate::app::action::ParseError::Unknown(_) => {
                        crate::error::SLASH_ERROR_UNKNOWN_COMMAND
                    }
                    crate::app::action::ParseError::Usage { .. } => {
                        crate::error::SLASH_ERROR_BAD_ARGUMENT
                    }
                };
                let hint = match error {
                    crate::app::action::ParseError::Unknown(_) => {
                        " Type /help to see the available commands."
                    }
                    crate::app::action::ParseError::Usage { .. } => "",
                };
                self.push_slash_error(format!("[error] code={code} msg={error}.{hint}"));
                return true;
            }
        };
        self.run_command(parsed, arg, on);
        true
    }

    /// Perform one parsed command. The arms are the terminal's half; the naming,
    /// the grammar and the availability behind them are the registry's.
    fn run_command(
        &mut self,
        parsed: crate::app::action::Command,
        arg: &str,
        on: &crate::ui::ConvKey,
    ) {
        use crate::app::action::{Call, Command, Read, TeamRead};
        use crate::app::command::Action;
        use crate::app::snapshot::{CatalogKind, ConfigSection, ResourceKind};
        match parsed {
            // -- reads: the state is structured, the view is the terminal's ----
            Command::Read(Read::Commands) => self.slash_help(),
            Command::Read(Read::Session) => self.slash_status(),
            Command::Read(Read::Context) => self.slash_context(),
            Command::Read(Read::Config(None)) => self.slash_config(),
            Command::Read(Read::Config(Some(ConfigSection::Permissions))) => {
                self.slash_permissions()
            }
            Command::Read(Read::Config(Some(ConfigSection::Mcp))) => {
                self.slash_mcp(McpRequest::List)
            }
            Command::Read(Read::Config(Some(_))) => self.slash_config(),
            Command::Read(Read::Catalog(CatalogKind::Models)) => self.open_model_menu(),
            Command::Read(Read::Catalog(CatalogKind::Providers)) => self.slash_provider(""),
            Command::Read(Read::Catalog(CatalogKind::Skills)) => self.slash_skills(),
            Command::Read(Read::Catalog(CatalogKind::McpServers)) => {
                self.slash_mcp(McpRequest::List)
            }
            Command::Read(Read::Catalog(CatalogKind::Images)) => self.slash_images(),
            Command::Read(Read::Resource(ResourceKind::Tasks)) => self.slash_tasks(),
            Command::Read(Read::Resource(_)) => self.slash_status(),
            Command::Read(Read::Sessions) => self.slash_resume(""),
            Command::Read(Read::Team(TeamRead::Chart)) => self.slash_team_read(TeamRead::Chart),
            Command::Read(Read::Team(TeamRead::Status)) => self.slash_team_read(TeamRead::Status),
            Command::Read(Read::Team(TeamRead::Validate)) => {
                self.slash_team_read(TeamRead::Validate)
            }
            Command::Read(Read::Team(TeamRead::Norms)) => self.slash_team_read(TeamRead::Norms),
            Command::Read(Read::Team(TeamRead::Memory)) => self.slash_team_read(TeamRead::Memory),
            // A picker is that action's own argument choices; which surface
            // shows them is the frontend's business.
            Command::Read(Read::Options(action)) => match action.as_str() {
                "theme.set" => self.open_theme_menu(),
                _ => self.open_think_menu(),
            },
            // -- lifecycle ----------------------------------------------------
            Command::Call(Call::Close) => self.exit = true,
            Command::Call(Call::Resume(locator)) => {
                let query = match &locator {
                    crate::app::snapshot::SessionLocator::Stem { stem } => stem.clone(),
                    _ => String::new(),
                };
                self.slash_resume(&query);
            }
            // -- actions ------------------------------------------------------
            Command::Act(action) => match action {
                Action::SessionReset => self.slash_clear(),
                Action::SessionRename { name } => self.slash_rename(&name),
                Action::SessionGarbageCollect => self.slash_gc(),
                // The console exports to its own default path, so `output` is
                // the wire's business and not this surface's.
                Action::SessionShare { public, open, .. } => self.slash_share(public, open),
                Action::SessionChangeDirectory { path } => self.slash_cd(&path.to_string_lossy()),
                Action::ConversationCompact { .. } => self.slash_compact(on),
                // esc-esc owns the checkpoint selector; a typed line never
                // reaches this arm.
                Action::ConversationRewind { .. } => self.slash_status(),
                Action::ModelSelect { model } => self.set_model(model),
                Action::ProviderSelect { provider } => self.slash_provider(&provider),
                // The one handler that keeps its own line, on purpose (B5 ruling
                // ③): `--device-auth` and `--manual <token>` are login mechanics
                // the action does not carry and must not — a pasted token is a
                // credential, and credentials have no business in a request. If
                // a GUI ever needs them it needs a design decision, not a field.
                Action::ProviderLogin { .. } | Action::ProviderLogout { .. } => {
                    self.slash_provider(arg)
                }
                Action::ThinkingSelect { level } => self.slash_think(level.as_str()),
                Action::PermissionModeSet { .. } => self.slash_status(),
                Action::PermissionRuleAdd { decision, rule } => {
                    self.permission_rule(decision, &rule, true)
                }
                Action::PermissionRuleRemove { decision, rule } => {
                    self.permission_rule(decision, &rule, false)
                }
                Action::McpEnable { server } => self.slash_mcp(McpRequest::SetEnabled {
                    target: server,
                    enabled: true,
                }),
                Action::McpDisable { server } => self.slash_mcp(McpRequest::SetEnabled {
                    target: server,
                    enabled: false,
                }),
                Action::McpReconnect { server } => self.slash_mcp(McpRequest::Reconnect { server }),
                Action::SkillInvoke { skill, input } => {
                    self.resubmit(crate::skills::invocation(&skill, input.as_deref()));
                }
                team @ (Action::TeamStart { .. }
                | Action::TeamAssign { .. }
                | Action::TeamStop { .. }
                | Action::TeamScaffold { .. }
                | Action::TeamMemoryGarbageCollect) => self.slash_team_act(&team),
                Action::RoomJoin { room } => self.slash_join(&room),
                Action::RoomLeave { room } => self.slash_leave(&room),
                Action::CommandPromote { .. } => {}
                Action::ThemeSet { theme } => {
                    self.apply_theme(ThemeSetting::parse(Some(theme.as_str())));
                }
            },
        }
    }

    fn slash_help(&mut self) {
        self.push_slash_info(crate::app::action::help_lines().join("\n"));
    }

    fn slash_cd(&mut self, arg: &str) {
        // Main's turn, not the page's: the directory is the console's setting
        // and a command that runs on every page (D135) must read the state it
        // actually disturbs.
        if self.main_conv().busy {
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
        let usage =
            crate::context_usage::ContextUsage::for_model(0, &self.session.client.models(), &model);
        self.main_conv().context_usage = usage;
    }

    /// Main's own occupancy: it is measured from main's transcript, so it is
    /// recorded against main's store wherever the screen happens to be.
    fn estimate_context_usage(&mut self, messages: &[crate::api::types::Message]) {
        let model = self.session.runtime.model.borrow().clone();
        let usage = crate::context_usage::ContextUsage::for_model(
            crate::compact::estimate_tokens(&self.session.system, messages, &[]),
            &self.session.client.models(),
            &model,
        );
        self.main_conv().context_usage = usage;
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

    pub(crate) fn rebind_tasks_to_transcript(
        &self,
        transcript: Option<&crate::transcript::Transcript>,
    ) {
        crate::engine::actions::bind_tasks(&self.session, transcript);
    }

    pub(crate) fn attach_share_to_transcript(
        &mut self,
        transcript: Option<&crate::transcript::Transcript>,
    ) {
        for warning in crate::engine::actions::bind_share(&self.session, transcript) {
            self.push_warning(warning);
        }
    }

    fn slash_clear(&mut self) {
        let done = crate::engine::actions::reset_session(&self.session);
        for warning in done.warnings {
            self.push_warning(warning);
        }
        let main = self.main_conv();
        main.messages.clear();
        main.stream_msg = None;
        main.stream_attempt_checkpoint = None;
        self.slash_lines.clear();
        self.warnings.clear();
        // The flush cursor belongs to the screen, and the screen may be
        // somebody else's page: resetting it there would reprint that page from
        // its top into scrollback, which write-once can never take back.
        if self.active.is_main() {
            self.reset_flushed();
        }
        self.reset_context_usage();
        self.say(done.said);
    }

    /// Switches the runtime model and persists it as the default (same path as /theme /think: writes the project layer).
    pub(crate) fn set_model(&mut self, model: String) {
        // Main's turn: the model is the console's setting (see `slash_cd`).
        if self.main_conv().busy {
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
        let unknown = match self.session.client.declared_models(&provider) {
            Some(declared) => !declared.iter().any(|entry| entry.id == model),
            None => self
                .models_cache
                .get(&provider)
                .is_some_and(|known| !known.is_empty() && !known.contains(&model)),
        };
        self.apply_to_core(crate::app::command::Action::ModelSelect {
            model: model.clone(),
        });
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

    /// Width a diff body gets inside an activity's expanded content, once
    /// `layout_activity` has prefixed [`RESULT_INDENT`].
    pub(crate) fn diff_width(&self) -> usize {
        self.width
            .saturating_sub(crate::tui::activities::RESULT_INDENT.len())
    }

    /// Re-render every diff activity under the current theme and width.
    ///
    /// Diff rows are baked when the edit lands, so they carry the palette and
    /// the gutter of that moment. `/theme` has to walk back over them, or the
    /// switch would recolour the prose and leave the diffs behind. Rows already
    /// committed to scrollback keep their old colours — that is the write-once
    /// contract, not a gap here.
    fn rebuild_diff_rows(&mut self) {
        let (theme, width) = (self.theme.clone(), self.diff_width());
        // Every store, not just the one on screen: `/theme` runs on any page
        // since D135, and a parked conversation the reader comes back to would
        // otherwise still be wearing the old palette.
        let messages = self
            .conv
            .messages
            .iter_mut()
            .chain(self.parked.values_mut().flat_map(|c| c.messages.iter_mut()));
        for message in messages {
            for activity in &mut message.activities {
                if let ActivityKind::Diff(d) = &activity.kind {
                    activity.content = diff_lines(d, &theme, width);
                }
            }
        }
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
        self.rebuild_diff_rows();
        self.dirty = true;
        let cwd = std::path::PathBuf::from(&self.cwd);
        let _ = crate::settings::upsert_scoped_settings(
            &self.session.user_config_dir,
            &cwd,
            &serde_json::json!({ "theme": name }),
        );
        self.push_slash_output(format!("✓ theme switched: {name}"));
    }

    /// `/images` — the session's content images, newest first.
    ///
    /// With nothing to show it says so on the info tier rather than opening an
    /// empty picker: a menu with no rows is a surface that answers a question
    /// by refusing to appear.
    pub(crate) fn slash_images(&mut self) {
        let entries = self.image_registry.newest_first();
        if entries.is_empty() {
            self.push_slash_info("no images in this session yet".to_string());
            return;
        }
        let ids: Vec<usize> = entries.iter().map(|e| e.id).collect();
        let items: Vec<crate::tui::picker::PickerItem> = entries
            .iter()
            .map(|e| crate::tui::picker::PickerItem::new(e.label(), e.id.to_string(), e.format))
            .collect();
        self.close_menus();
        self.images_menu = Some(ImagesMenu {
            selected: 0,
            ids,
            items,
        });
        self.clear_slash_suggestions();
    }

    /// Images menu keys: ↑↓/1-9 move, Enter opens the browsed image in the
    /// desktop's viewer, Esc closes. Returns whether consumed.
    pub(crate) fn images_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(menu) = &mut self.images_menu else {
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
                // Swallowed even out of range: a menu is a modal surface.
                true
            }
            KeyCode::Enter => {
                let id = menu.ids.get(menu.selected).copied();
                self.images_menu = None;
                if let Some(id) = id {
                    self.open_image(id);
                }
                self.dirty = true;
                true
            }
            KeyCode::Esc => {
                self.images_menu = None;
                self.dirty = true;
                true
            }
            _ => false,
        }
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
        // Empty-table guard (the theme vocabulary is never empty; this defensive branch is unreachable).
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

    /// Async stats shared by /status and /context: message count + token count.
    fn slash_stats_async(&mut self, format: impl Fn(usize, u64) -> String + Send + 'static) {
        let session = self.session.clone();
        let events = self.events.clone();
        self.pin_panel("stats", vec!["⏳ gathering stats…".to_string()]);
        tokio::spawn(async move {
            let unpin = || {
                events.send(UiEvent::Unpin {
                    id: "stats".to_string(),
                });
            };
            let model = session.runtime.model.borrow().clone();
            let transcript = session.runtime.transcript.borrow().clone();
            let msgs = transcript
                .map(|t| t.load_messages().unwrap_or_default())
                .unwrap_or_default();
            // Count with the tool schemas each request carries — the same payload
            // the auto-compact gate measures (query_loop), so /status and /context
            // report the number the gate acts on.
            let mut warn = |_: String| {};
            let tools = crate::tools::assemble_tools(&session, &mut warn).await;
            let schemas = crate::tool::tool_params(&tools);
            let tokens = match session
                .client
                .count_tokens(&model, &session.system, &msgs, &schemas)
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    // #18/main #91: short-op failures must be visible (page-level error row),
                    // behavior keeps degrading gracefully (budget still shows 0).
                    events.send(UiEvent::Error {
                        code: crate::error::map_error(&e).to_string(),
                        msg: e.to_string(),
                        level: crate::error::ErrorLevel::Page,
                        context: crate::error::ErrorContext::ShortSync,
                    });
                    0
                }
            };
            unpin();
            events.send(UiEvent::SlashInfo(format(msgs.len(), tokens)));
        });
    }
}

pub(crate) fn one_line(text: &str, width: usize) -> String {
    let flat = crate::tui::line::sanitize(text);
    crate::tui::markdown::truncate(flat.as_ref(), width.max(1))
}

pub(crate) fn user_message_rows(text: &str, width: usize, theme: &Theme) -> Vec<Row> {
    // An interrupt marker is a user-role message the user never wrote: the harness
    // recorded it so the model learns the turn was cut off. It reads as a state line in
    // the error colour, never as a `❯` bubble putting words in the user's mouth.
    // A failed agent (D98) joins it: the one line an agent's life still writes
    // into this flow, and it is bad news, so it wears the error tier rather than
    // the dim one every other unspoken line settles into.
    if crate::query::is_interrupt_marker(text) || crate::tui::bufferview::is_agent_alert(text) {
        return vec![Row::new(Line::styled(
            crate::tui::markdown::truncate(text, width.max(1)),
            SegStyle::fg(theme.error),
        ))];
    }
    // A message the running turn absorbed at a tool barrier (D83). The user did write
    // it, so it keeps its send stamp — but it did not open a turn, it landed inside
    // one, and the `↪` line says exactly that: the reply above was written without it,
    // the reply below with it.
    if is_steer_line(text) {
        return vec![Row::new(Line::styled(
            crate::tui::markdown::truncate(text, width.max(1)),
            theme.dim(),
        ))];
    }
    // A dialog the turn outlived, or the receipt of one the user answered:
    // nothing failed, so they settle dim rather than in the error colour, and
    // neither wears the `❯` bubble — the user answered a question, they did not
    // write this line.
    if text == ASK_CANCELLED_TEXT || is_ask_receipt(text) {
        return vec![Row::new(Line::styled(
            crate::tui::markdown::truncate(text, width.max(1)),
            theme.dim(),
        ))];
    }
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

#[path = "chat_menus.rs"]
mod chat_menus;

#[path = "chat_session.rs"]
mod chat_session;

#[path = "chat_setup.rs"]
mod chat_setup;

#[path = "rewind_ui.rs"]
pub mod rewind_ui;

/// The `/resume` selector model, rendered by chrome.rs.
pub use chat_session::ResumeMenu;

/// The shape `/mcp` reaches its handler in, declared beside it.
use chat_setup::McpRequest;

/// The overlay frame the ctrl+b manager draws, reused by the rewind selector
/// (D91) so the two overlays are the same object in the same place.
pub(crate) use chat_tail::manager_box;

#[path = "chat_feed.rs"]
mod chat_feed;

#[path = "ask.rs"]
mod ask;

#[cfg(test)]
pub(crate) use crate::tui::motion::update_color;
#[cfg(test)]
pub(crate) use chat_tail::{banner_line, banner_segments, welcome_card_rows};

#[cfg(test)]
#[path = "chat_tests_a.rs"]
mod tests_a;

#[cfg(test)]
#[path = "chat_tests_b.rs"]
mod tests_b;

#[cfg(test)]
#[path = "chat_tests_c.rs"]
mod tests_c;

#[cfg(test)]
#[path = "chat_tests_d.rs"]
mod tests_d;

#[cfg(test)]
#[path = "chat_tests_e.rs"]
mod tests_e;

#[cfg(test)]
#[path = "chat_tests_f.rs"]
mod tests_f;
