//! Completion sources for the composer: the fuzzy scorer, the `@`/`#` typeahead
//! and slash **argument** completion (D85, D103).
//!
//! Three rules shape this module:
//!
//! - **One scorer.** [`fuzzy_score`] is the only ranking function; the mention
//!   dropdown and the argument dropdown both go through [`fuzzy_rank`], so a
//!   query behaves identically wherever it is typed.
//! - **One registry.** [`super::chat::Chat::arg_candidates`] is the single
//!   `match` that maps a command to its argument domain. Every arm reads the
//!   *same* data its handler validates against — a parallel hardcoded list
//!   would drift the moment a provider or a theme is added.
//! - **Gather once.** The file list is collected when the dropdown opens, not
//!   per keystroke: [`MentionState::all`] is the snapshot, [`MentionState::items`]
//!   the filtered view of it.
//!
//! **The sigil at the start of a line means something else** (D103). There, `@`
//! and `#` open a direct send — the message goes to that agent or that room
//! instead of to the model — so the dropdown offers exactly what the send can
//! reach, with what it will do written beside it. Anywhere else in a line `@`
//! is the file-and-agent reference it has always been, and `#` is prose.

use std::path::Path;

use crate::tui::chat::{Chat, Row};
use crate::tui::line::{Line, SegStyle};
use crate::tui::theme::Theme;

/// Score for a match that continues the previous matched character.
const CONSECUTIVE_BONUS: i32 = 8;
/// Score for a match at a word or path boundary (`/`, `_`, `-`, `.`, camelCase…).
const BOUNDARY_BONUS: i32 = 10;
/// Base score per matched character.
const MATCH_BASE: i32 = 1;
/// Ceiling on the "started late" penalty, so a deep path can still win on
/// quality instead of losing on depth alone.
const MAX_LEAD_PENALTY: i32 = 16;

/// Upper bound on the files offered by `@`. A repository larger than this is
/// listed partially and says so in the dropdown footer — an unbounded walk of
/// a monorepo would stall the composer.
pub const MENTION_FILE_CAP: usize = 5000;
/// Depth limit for the non-git fallback walk.
const WALK_MAX_DEPTH: usize = 6;
/// Rows the mention dropdown shows before it starts counting the rest.
pub const MENTION_MAX_ITEMS: usize = 10;
/// Of those rows, the most that agents may take (they are few and specific;
/// files must not be able to push them off the list entirely).
const MENTION_MAX_AGENTS: usize = 4;

/// Directory names skipped by the non-git walk. Hidden directories are skipped
/// separately, by the leading dot.
const WALK_SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    "venv",
];

// ---------------------------------------------------------------------------
// Fuzzy scorer
// ---------------------------------------------------------------------------

/// ASCII-only case folding.
///
/// Deliberate: `char::to_lowercase` may expand one character into several,
/// which would break the 1:1 index alignment the scorer needs between the
/// folded and the original candidate. Identifiers, model ids and paths — every
/// thing this scorer ranks — are ASCII; non-ASCII characters simply match
/// case-sensitively.
fn fold(c: char) -> char {
    c.to_ascii_lowercase()
}

/// Whether position `i` in `chars` starts a word or a path segment.
fn is_boundary(chars: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let Some(&prev) = chars.get(i - 1) else {
        return false;
    };
    if matches!(prev, '/' | '\\' | '_' | '-' | '.' | ' ' | ':' | '@') {
        return true;
    }
    // camelCase: the capital that follows a lowercase letter or a digit.
    let Some(&here) = chars.get(i) else {
        return false;
    };
    (prev.is_lowercase() || prev.is_ascii_digit()) && here.is_uppercase()
}

/// Case-insensitive subsequence score: `None` when `query` is not a
/// subsequence of `candidate`, otherwise a rank where higher is better.
///
/// The match is found in two passes — a forward pass for the *earliest* end
/// position, then a backward pass from that end for the *tightest* set of
/// positions reaching it. A single greedy pass would score `ab` against
/// `axab` on the scattered match and miss the adjacent one.
///
/// An empty query matches everything with score `0`, which is what lets a
/// freshly opened dropdown show its source list untouched.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    let needle: Vec<char> = query.chars().map(fold).collect();
    if needle.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = candidate.chars().collect();
    let folded: Vec<char> = chars.iter().copied().map(fold).collect();
    if needle.len() > folded.len() {
        return None;
    }

    // Forward: the earliest index at which the whole query has been consumed.
    let mut next = 0usize;
    let mut end = None;
    for (i, &c) in folded.iter().enumerate() {
        if c == needle[next] {
            next += 1;
            if next == needle.len() {
                end = Some(i + 1);
                break;
            }
        }
    }
    let end = end?;

    // Backward from that end: the latest possible position for each query
    // character, i.e. the tightest match ending where the forward pass landed.
    let mut positions = vec![0usize; needle.len()];
    let mut remaining = needle.len();
    let mut i = end;
    while i > 0 && remaining > 0 {
        i -= 1;
        if folded[i] == needle[remaining - 1] {
            remaining -= 1;
            positions[remaining] = i;
        }
    }
    if remaining != 0 {
        return None;
    }

    let mut score = 0i32;
    for (k, &pos) in positions.iter().enumerate() {
        score += MATCH_BASE;
        if k > 0 && positions[k - 1] + 1 == pos {
            score += CONSECUTIVE_BONUS;
        }
        if is_boundary(&chars, pos) {
            score += BOUNDARY_BONUS;
        }
    }
    // Earlier beats later, but only up to a point (see MAX_LEAD_PENALTY).
    let lead = i32::try_from(positions[0]).unwrap_or(MAX_LEAD_PENALTY);
    Some(score - lead.min(MAX_LEAD_PENALTY))
}

/// Filters and ranks `items` by `query` using [`fuzzy_score`], keyed by `key`.
///
/// An **empty query keeps the source order**: a catalog lists its preferred
/// entry first and the session list lists the most recent session first, and
/// re-sorting an unfiltered list lexically would throw that away. With a
/// non-empty query the order is score-descending, ties broken lexically so the
/// result never depends on the input order.
pub fn fuzzy_rank<T>(query: &str, items: Vec<T>, key: impl Fn(&T) -> &str) -> Vec<T> {
    if query.is_empty() {
        return items;
    }
    let mut scored: Vec<(i32, T)> = items
        .into_iter()
        .filter_map(|item| fuzzy_score(query, key(&item)).map(|score| (score, item)))
        .collect();
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| key(&left.1).cmp(key(&right.1)))
    });
    scored.into_iter().map(|(_, item)| item).collect()
}

// ---------------------------------------------------------------------------
// `@` mention
// ---------------------------------------------------------------------------

/// What a typeahead row refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionKind {
    /// A file relative to the session working directory.
    File,
    /// A background/team agent.
    Agent,
    /// A room, offered only at the start of a line (D103).
    Room,
}

impl MentionKind {
    /// Section header shown above this kind's rows.
    pub fn section(self) -> &'static str {
        match self {
            MentionKind::File => "Files",
            MentionKind::Agent => "Agents",
            MentionKind::Room => "Rooms",
        }
    }
}

/// One typeahead candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionItem {
    /// Relative path (file), agent name, or room name.
    pub value: String,
    pub kind: MentionKind,
    /// What this row does and what state it is in — `send message · running`
    /// for an agent at the start of a line, empty for a plain reference. It is
    /// the only thing that tells a direct send apart from a file reference
    /// before the user has pressed Enter.
    pub note: String,
}

impl MentionItem {
    fn new(value: impl Into<String>, kind: MentionKind) -> Self {
        Self {
            value: value.into(),
            kind,
            note: String::new(),
        }
    }

    fn noted(value: impl Into<String>, kind: MentionKind, note: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            kind,
            note: note.into(),
        }
    }

    /// What selecting this row writes into the composer, without the trailing
    /// space. A file drops the sigil (it becomes a path the model can read);
    /// an agent and a room keep theirs, because `@name` / `#name` is the token
    /// the direct send reads.
    pub fn insertion(&self) -> String {
        match self.kind {
            MentionKind::File => self.value.clone(),
            MentionKind::Agent => format!("@{}", self.value),
            MentionKind::Room => format!("#{}", self.value),
        }
    }
}

/// The open `@` dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionState {
    /// Byte offset of the sigil in the composer — the replacement point.
    pub start: usize,
    /// Which sigil opened it: `@` or `#`.
    pub sigil: char,
    /// Text typed after the `@`.
    pub query: String,
    /// Everything gathered when the dropdown opened (never re-gathered while
    /// it stays open on the same token).
    pub all: Vec<MentionItem>,
    /// `all` filtered and ranked by `query`, capped for display.
    pub items: Vec<MentionItem>,
    /// Number of matches beyond `items`.
    pub more: usize,
    /// The file source hit [`MENTION_FILE_CAP`].
    pub truncated: bool,
    pub selected: usize,
}

/// The typeahead token under the caret, as `(byte offset, sigil, query)`.
///
/// Opens only at a word boundary — the start of the input or right after
/// whitespace — which is what keeps `user@example.com` an email address
/// instead of a mention of `example.com`.
///
/// `#` opens **only at the start of the line**, where it is the direct send's
/// own sigil. Mid-line it is a hash in a sentence — an issue number, a colour,
/// a heading in a pasted block — and a dropdown over any of those would be
/// noise (D103).
pub fn mention_token(input: &str, cursor: usize) -> Option<(usize, char, &str)> {
    let cursor = cursor.min(input.len());
    if !input.is_char_boundary(cursor) {
        return None;
    }
    let head = &input[..cursor];
    // The token under the caret runs back to the last whitespace.
    let start = head
        .rfind(char::is_whitespace)
        .map(|i| i + head[i..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(0);
    let token = &head[start..];
    let sigil = token.chars().next()?;
    if sigil == '#' && start > 0 {
        return None;
    }
    if sigil != '@' && sigil != '#' {
        return None;
    }
    Some((start, sigil, &token[sigil.len_utf8()..]))
}

/// Project files relative to `cwd`, plus whether the list was cut at `cap`.
///
/// Inside a git repository the source is git itself
/// (`git ls-files --cached --others --exclude-standard -z`), so `.gitignore`
/// is honoured for free and the answer matches what the user thinks "the
/// project" is. Everywhere else a bounded walk stands in.
pub fn project_files(cwd: &Path, cap: usize) -> (Vec<String>, bool) {
    if let Some(found) = git_files(cwd, cap) {
        return found;
    }
    walk_files(cwd, cap)
}

/// `git ls-files` in `cwd`; `None` when this is not a git repository (or git
/// is not installed), which is the caller's signal to fall back.
fn git_files(cwd: &Path, cap: usize) -> Option<(Vec<String>, bool)> {
    let output = std::process::Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut files: Vec<String> = Vec::new();
    let mut truncated = false;
    for entry in text.split('\0').filter(|s| !s.is_empty()) {
        if files.len() >= cap {
            truncated = true;
            break;
        }
        files.push(entry.to_string());
    }
    Some((files, truncated))
}

/// Bounded recursive walk for non-repositories: depth ≤ [`WALK_MAX_DEPTH`],
/// no hidden directories, no [`WALK_SKIP_DIRS`], `/`-joined relative paths on
/// every platform so a completion looks the same as git's.
fn walk_files(root: &Path, cap: usize) -> (Vec<String>, bool) {
    let mut files: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut queue: Vec<(std::path::PathBuf, String, usize)> =
        vec![(root.to_path_buf(), String::new(), 0)];
    while let Some((dir, prefix, depth)) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<(std::path::PathBuf, String, usize)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth + 1 > WALK_MAX_DEPTH || WALK_SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                children.push((entry.path(), rel, depth + 1));
            } else {
                if files.len() >= cap {
                    truncated = true;
                    return (files, truncated);
                }
                files.push(rel);
            }
        }
        queue.extend(children);
    }
    files.sort();
    (files, truncated)
}

/// Mention dropdown rows: section headers, the ranked rows, and one footer
/// that carries both the keys and whatever the list is not showing.
pub fn mention_rows(state: &MentionState, theme: &Theme, width: usize) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut section: Option<MentionKind> = None;
    for (i, item) in state.items.iter().enumerate() {
        if section != Some(item.kind) {
            section = Some(item.kind);
            rows.push(Row::new(Line::styled(
                format!("  {}", item.kind.section()),
                SegStyle::fg(theme.text_secondary),
            )));
        }
        let selected = i == state.selected;
        let color = if selected {
            theme.permission
        } else {
            theme.text_secondary
        };
        let note = if item.note.is_empty() {
            String::new()
        } else {
            format!(" · {}", item.note)
        };
        let line = crate::tui::markdown::truncate(
            &format!(
                "{}{}{note}",
                if selected { "❯ " } else { "  " },
                item.insertion()
            ),
            width.saturating_sub(2),
        );
        rows.push(Row::new(Line::styled(line, SegStyle::fg(color))));
    }
    let mut hint = if state.items.is_empty() {
        match state.sigil {
            '#' => "(no matching room)".to_string(),
            _ => "(no matching file or agent)".to_string(),
        }
    } else {
        "↑↓ select · tab/enter inserts · esc closes".to_string()
    };
    if state.more > 0 {
        hint.push_str(&format!(" · {} more", state.more));
    }
    if state.truncated {
        hint.push_str(&format!(" · first {MENTION_FILE_CAP} files only"));
    }
    rows.push(Row::new(Line::styled(
        format!(
            "  {}",
            crate::tui::markdown::truncate(&hint, width.saturating_sub(2))
        ),
        SegStyle::fg(theme.text_secondary),
    )));
    rows
}

// ---------------------------------------------------------------------------
// Slash argument completion
// ---------------------------------------------------------------------------

/// One argument candidate for a slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgCandidate {
    pub value: String,
    pub description: String,
}

impl ArgCandidate {
    pub fn new(value: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            description: description.into(),
        }
    }
}

/// A slash line that has moved past its command name: `/<command> <done…> <partial>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgContext<'a> {
    pub command: &'a str,
    /// Argument tokens already finished (whitespace-terminated).
    pub done: Vec<&'a str>,
    /// The token being typed; empty right after a space.
    pub partial: &'a str,
    /// Byte offset where `partial` starts — where a completion is spliced in.
    pub start: usize,
}

/// Parses the argument phase out of the composer, or `None` while the user is
/// still typing the command *name* (which is the existing dropdown's job).
pub fn arg_context(input: &str) -> Option<ArgContext<'_>> {
    let rest = input.strip_prefix('/')?;
    if rest.contains('\n') {
        return None;
    }
    // No whitespace yet = still naming the command.
    let split = rest.find(char::is_whitespace)?;
    let command = &rest[..split];
    if command.is_empty() {
        return None;
    }
    let args = &rest[split..];
    let base = 1 + split;
    // `args` starts with the whitespace we split on, so this always matches.
    let last_ws = args.rfind(char::is_whitespace)?;
    let ws_len = args[last_ws..].chars().next().map_or(1, char::len_utf8);
    let head = &args[..last_ws];
    let partial = &args[last_ws + ws_len..];
    Some(ArgContext {
        command,
        done: head.split_whitespace().collect(),
        partial,
        start: base + last_ws + ws_len,
    })
}

impl Chat {
    // -- mention -----------------------------------------------------------

    /// Opens, refreshes or closes the `@` dropdown from the current composer
    /// state. Called from `update_slash_suggestions`, so every edit path that
    /// already refreshed the slash dropdown refreshes this one too.
    pub(crate) fn update_mention(&mut self) {
        // A permission dialog owns the keyboard (D80/D81): nothing typed
        // behind it may open a surface that competes for Enter.
        let token = if self.pending_ask.is_some() {
            None
        } else {
            mention_token(&self.input, self.cursor)
        };
        let Some((start, sigil, query)) = token else {
            self.mention = None;
            self.mention_dismissed = false;
            return;
        };
        let query = query.to_string();
        if self.mention_dismissed {
            return;
        }
        let reopen = match &self.mention {
            Some(state) => state.start != start || state.sigil != sigil,
            None => true,
        };
        if reopen {
            let (all, truncated) = self.gather_mentions(start == 0, sigil);
            self.mention = Some(MentionState {
                start,
                sigil,
                query: String::new(),
                all,
                items: Vec::new(),
                more: 0,
                truncated,
                selected: 0,
            });
        }
        let Some(state) = self.mention.as_mut() else {
            return;
        };
        state.query = query;
        let ranked = fuzzy_rank(&state.query, state.all.clone(), |item| item.value.as_str());
        let (people, files): (Vec<MentionItem>, Vec<MentionItem>) = ranked
            .into_iter()
            .partition(|item| item.kind != MentionKind::File);
        // Mid-line, files lead — that is what `@` is for there — and the
        // participants keep a reserved tail so a big repository cannot push
        // them off the list. At the start of a line the order flips: the rows
        // whose Enter bypasses the model are the ones being chosen between, and
        // they get the whole list if no file matches.
        let at_line_start = state.start == 0;
        let people_rows = if at_line_start {
            people.len().min(MENTION_MAX_ITEMS)
        } else {
            people.len().min(MENTION_MAX_AGENTS)
        };
        let file_rows = files.len().min(MENTION_MAX_ITEMS - people_rows);
        let total = people.len() + files.len();
        let mut items: Vec<MentionItem> = Vec::with_capacity(people_rows + file_rows);
        if at_line_start {
            items.extend(people.into_iter().take(people_rows));
            items.extend(files.into_iter().take(file_rows));
        } else {
            items.extend(files.into_iter().take(file_rows));
            items.extend(people.into_iter().take(people_rows));
        }
        state.more = total - items.len();
        state.selected = state.selected.min(items.len().saturating_sub(1));
        state.items = items;
    }

    /// Collects the dropdown's contents once. Returns the items and whether the
    /// file source hit its cap.
    ///
    /// **`#` at the start of a line is rooms and nothing else**: that sigil has
    /// exactly one meaning and it is the direct send's.
    ///
    /// **`@` at the start of a line leads with the send targets** and each row
    /// says what pressing Enter will do — `@scout · send message · running` —
    /// because that is the one position where the line is about to bypass the
    /// model. Every instance is listed, stopped ones included: a message
    /// resumes a stopped instance, which is CC's subagent semantics and already
    /// bingo's delivery path, so a list that hid them would refuse to offer
    /// something the send can do.
    ///
    /// **Files stay in that list, under the agents.** `@src/lexer.rs why does
    /// this loop?` is an ordinary prompt that happens to start with the file
    /// sigil, and the send only fires on a name that resolves to an instance —
    /// so the two grammars do not actually collide, and dropping files here
    /// would take away a reference for a conflict that does not exist.
    ///
    /// Anywhere else it is the D85 reference dropdown, unchanged: project files
    /// with the running agents on the tail.
    fn gather_mentions(&self, at_line_start: bool, sigil: char) -> (Vec<MentionItem>, bool) {
        if sigil == '#' {
            let rooms = self
                .session
                .channels
                .list()
                .into_iter()
                .map(|status| {
                    let note = if status
                        .members
                        .iter()
                        .any(|m| m == crate::channels::USER_NAME)
                    {
                        "post to room".to_string()
                    } else {
                        "post to room · joins you".to_string()
                    };
                    MentionItem::noted(status.name, MentionKind::Room, note)
                })
                .collect();
            return (rooms, false);
        }
        let (files, truncated) = project_files(Path::new(&self.cwd), MENTION_FILE_CAP);
        let mut items: Vec<MentionItem> = if at_line_start {
            self.session
                .agents
                .list()
                .into_iter()
                .map(|status| {
                    MentionItem::noted(
                        status.name,
                        MentionKind::Agent,
                        format!("send message · {}", status.state.label()),
                    )
                })
                .collect()
        } else {
            self.session
                .agents
                .list()
                .into_iter()
                .filter(|status| status.state == crate::agents::AgentState::Running)
                .map(|status| MentionItem::new(status.name, MentionKind::Agent))
                .collect()
        };
        items.extend(
            files
                .into_iter()
                .map(|value| MentionItem::new(value, MentionKind::File)),
        );
        (items, truncated)
    }

    /// Mention dropdown keys: ↑↓ move, Tab/Enter insert, Esc closes.
    /// Returns true = consumed.
    pub(crate) fn mention_menu_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        let Some(state) = self.mention.as_mut() else {
            return false;
        };
        if state.items.is_empty() {
            // An empty dropdown still owns Esc, so the footer's promise holds.
            if code == KeyCode::Esc {
                self.close_mention();
                return true;
            }
            return false;
        }
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                state.selected = (state.selected + 1) % state.items.len();
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                state.selected = state
                    .selected
                    .checked_sub(1)
                    .unwrap_or(state.items.len() - 1);
                true
            }
            KeyCode::Tab | KeyCode::Enter => {
                self.apply_mention();
                true
            }
            KeyCode::Esc => {
                self.close_mention();
                true
            }
            _ => false,
        }
    }

    /// Dismisses the dropdown without touching the text. The dismissal sticks
    /// until the caret leaves the token — otherwise the next keystroke would
    /// reopen what Esc just closed.
    pub(crate) fn close_mention(&mut self) {
        self.mention = None;
        self.mention_dismissed = true;
        self.dirty = true;
    }

    /// Replaces the `@query` token with the selection plus one space.
    fn apply_mention(&mut self) {
        let Some(state) = self.mention.as_ref() else {
            return;
        };
        let Some(item) = state.items.get(state.selected) else {
            return;
        };
        let start = state.start.min(self.input.len());
        let end = self.cursor.min(self.input.len());
        if !self.input.is_char_boundary(start) || !self.input.is_char_boundary(end) || end < start {
            return;
        }
        let mut text = self.input[..start].to_string();
        text.push_str(&item.insertion());
        text.push(' ');
        let cursor = text.len();
        text.push_str(&self.input[end..]);
        self.input = text;
        self.cursor = cursor;
        self.mention = None;
        self.mention_dismissed = false;
        self.dirty = true;
    }

    // -- slash arguments ---------------------------------------------------

    /// The one registry of argument-completion sources.
    ///
    /// Every arm reads the data its own handler validates against, so a
    /// candidate the dropdown offers is a value the command accepts. `None`
    /// means "this argument is free-form" (`/cd`, `/rename`, `/team message …`)
    /// and no dropdown opens.
    pub(crate) fn arg_candidates(&self, ctx: &ArgContext<'_>) -> Option<Vec<ArgCandidate>> {
        match (ctx.command, ctx.done.as_slice()) {
            ("model", []) => Some(self.model_candidates()),
            ("theme", []) => Some(
                crate::tui::chat::THEME_LEVELS
                    .iter()
                    .map(|(name, desc)| ArgCandidate::new(*name, *desc))
                    .collect(),
            ),
            ("think", []) => Some(
                crate::tui::chat::THINK_LEVELS
                    .iter()
                    .map(|(name, desc)| ArgCandidate::new(*name, *desc))
                    .collect(),
            ),
            ("resume", []) => Some(self.resume_candidates()),
            // `/provider` is two-shaped: a bare argument switches provider, and
            // `login`/`logout` take one. Offer both at the first token.
            ("provider", []) => {
                let mut items = vec![
                    ArgCandidate::new("login", "authenticate a provider"),
                    ArgCandidate::new("logout", "drop a provider's stored credentials"),
                ];
                items.extend(
                    self.provider_order()
                        .into_iter()
                        .map(|name| ArgCandidate::new(name.clone(), self.provider_desc(&name))),
                );
                Some(items)
            }
            ("provider", ["login"] | ["logout"]) => Some(self.login_provider_candidates()),
            // The registry itself, so a name the dropdown offers is a
            // conversation that exists (D89).
            _ => None,
        }
    }

    /// Model ids for the current provider, from the same three synchronous
    /// tiers the `/model` picker's level two uses, in the same order —
    /// declared catalog, this session's fetched list, a fresh disk cache. The
    /// picker's fourth tier is a network fetch, which a dropdown refreshed on
    /// every keystroke must not do.
    fn model_candidates(&self) -> Vec<ArgCandidate> {
        let provider = self.session.runtime.provider.borrow().clone();
        if let Some(declared) = self.session.client.declared_models(&provider) {
            return declared
                .iter()
                .map(|entry| {
                    let label = entry.label();
                    let desc = if label == entry.id { "" } else { label };
                    ArgCandidate::new(entry.id.clone(), desc)
                })
                .collect();
        }
        if let Some(known) = self.models_cache.get(&provider)
            && !known.is_empty()
        {
            return known
                .iter()
                .map(|id| ArgCandidate::new(id.clone(), ""))
                .collect();
        }
        let Some((_, base_url)) = self.session.client.provider_endpoint(&provider) else {
            return Vec::new();
        };
        crate::model_cache::ModelCache::new(&self.session.home)
            .get(&provider, &base_url)
            .filter(|cached| cached.fresh())
            .map(|cached| {
                cached
                    .models
                    .iter()
                    .map(|id| ArgCandidate::new(id.clone(), ""))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Session names, from the same untruncated listing `/resume <keyword>`
    /// searches (the 20-row cap belongs to the picker, not to the matcher).
    fn resume_candidates(&self) -> Vec<ArgCandidate> {
        crate::transcript::list(&self.session.home)
            .unwrap_or_default()
            .iter()
            .map(|transcript| ArgCandidate::new(transcript.name(), ""))
            .collect()
    }

    /// The names `/provider login|logout` accepts: configured providers plus
    /// the compile-time presets. Deliberately *not* `provider_order()` —
    /// that one includes `default`, which login rejects.
    fn login_provider_candidates(&self) -> Vec<ArgCandidate> {
        let mut names: Vec<String> = self.session.settings.providers.keys().cloned().collect();
        names.extend(
            crate::api::providers::presets::PRESETS
                .iter()
                .map(|preset| preset.name.to_string()),
        );
        names.sort();
        names.dedup();
        names
            .into_iter()
            .map(|name| {
                let desc = self.provider_desc(&name);
                ArgCandidate::new(name, desc)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn best(query: &str, candidates: &[&str]) -> String {
        let ranked = fuzzy_rank(
            query,
            candidates.iter().map(|s| s.to_string()).collect(),
            |s| s.as_str(),
        );
        ranked.first().cloned().unwrap_or_default()
    }

    #[test]
    fn fuzzy_requires_a_subsequence() {
        assert!(fuzzy_score("abc", "a_b_c").is_some());
        assert!(fuzzy_score("cba", "a_b_c").is_none());
        assert!(fuzzy_score("abcd", "abc").is_none());
    }

    #[test]
    fn fuzzy_is_case_insensitive() {
        assert_eq!(fuzzy_score("MoDeL", "model"), fuzzy_score("model", "model"));
        assert!(fuzzy_score("TUI", "src/tui/chat.rs").is_some());
    }

    #[test]
    fn fuzzy_prefers_consecutive_over_scattered() {
        let consecutive = fuzzy_score("abc", "abcxyz");
        let scattered = fuzzy_score("abc", "axbxcz");
        assert!(consecutive > scattered, "{consecutive:?} vs {scattered:?}");
        assert_eq!(best("abc", &["axbxcz", "abcxyz"]), "abcxyz");
    }

    #[test]
    fn fuzzy_prefers_word_and_path_boundaries() {
        let boundary = fuzzy_score("b", "a/b");
        let midword = fuzzy_score("b", "ab");
        assert!(boundary > midword, "{boundary:?} vs {midword:?}");
        assert_eq!(best("md", &["commander", "src/models.rs"]), "src/models.rs");
    }

    #[test]
    fn fuzzy_prefers_earlier_matches() {
        let early = fuzzy_score("a", "a-x-y");
        let late = fuzzy_score("a", "x-y-a");
        assert!(early > late, "{early:?} vs {late:?}");
    }

    #[test]
    fn fuzzy_finds_the_tightest_match_not_the_first_one() {
        // `xaxab` carries a decoy `a` at index 1. Greedy-forward alone would
        // take it and score a scattered a@1+b@4; the backward pass tightens
        // onto a@3+b@4, so the decoy cannot change the answer — the score is
        // identical to the same tail with no decoy at all.
        assert_eq!(fuzzy_score("ab", "xaxab"), fuzzy_score("ab", "xxxab"));
        // And that tightened match is worth more than a genuinely scattered one.
        let tight = fuzzy_score("ab", "xaxab");
        let scattered = fuzzy_score("ab", "xaxxb");
        assert!(tight > scattered, "{tight:?} vs {scattered:?}");
    }

    /// The reason tightening exists: inside a path, the segment the user is
    /// aiming at is rarely the first place its letters happen to appear.
    #[test]
    fn fuzzy_tightening_finds_the_path_segment() {
        let real = fuzzy_score("ch", "src/chat.rs");
        let coincidence = fuzzy_score("ch", "circus/hat.rs");
        assert!(real > coincidence, "{real:?} vs {coincidence:?}");
        assert_eq!(best("ch", &["circus/hat.rs", "src/chat.rs"]), "src/chat.rs");
    }

    #[test]
    fn empty_query_matches_everything_in_source_order() {
        let source = vec!["zebra".to_string(), "alpha".to_string(), "m".to_string()];
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert_eq!(fuzzy_rank("", source.clone(), |s| s.as_str()), source);
    }

    #[test]
    fn ranking_ties_break_lexically() {
        let ranked = fuzzy_rank("x", vec!["bx".to_string(), "ax".to_string()], |s| {
            s.as_str()
        });
        assert_eq!(ranked, vec!["ax".to_string(), "bx".to_string()]);
    }

    #[test]
    fn mention_token_opens_at_a_word_boundary_only() {
        assert_eq!(mention_token("@src", 4), Some((0, '@', "src")));
        assert_eq!(mention_token("look at @src/m", 14), Some((8, '@', "src/m")));
        assert_eq!(mention_token("@", 1), Some((0, '@', "")));
        // An email address is not a mention.
        assert_eq!(mention_token("user@example.com", 16), None);
        assert_eq!(mention_token("a@b", 3), None);
        // No token under the caret.
        assert_eq!(mention_token("plain text", 10), None);
        // The caret is what anchors it: before the `@` there is no token.
        assert_eq!(mention_token("@src", 0), None);
    }

    /// D103: `#` is the direct send's other sigil, and it opens **only** at the
    /// start of a line. Mid-line a hash is a hash — an issue number, a colour, a
    /// heading in a pasted block — and a dropdown over any of those is noise.
    #[test]
    fn a_hash_opens_the_typeahead_only_at_the_start_of_a_line() {
        assert_eq!(mention_token("#bui", 4), Some((0, '#', "bui")));
        assert_eq!(mention_token("#", 1), Some((0, '#', "")));
        assert_eq!(mention_token("see #42", 7), None);
        assert_eq!(mention_token("fix #42 first", 7), None);
    }

    #[test]
    fn arg_context_splits_command_done_and_partial() {
        assert_eq!(arg_context("/model"), None, "still naming the command");
        assert_eq!(arg_context("hello"), None);

        let ctx = arg_context("/model ").expect("argument phase");
        assert_eq!(ctx.command, "model");
        assert!(ctx.done.is_empty());
        assert_eq!(ctx.partial, "");
        assert_eq!(ctx.start, "/model ".len());

        let ctx = arg_context("/model dee").expect("argument phase");
        assert_eq!(ctx.partial, "dee");
        assert_eq!(ctx.start, "/model ".len());

        let ctx = arg_context("/provider login co").expect("argument phase");
        assert_eq!(ctx.command, "provider");
        assert_eq!(ctx.done, vec!["login"]);
        assert_eq!(ctx.partial, "co");
        assert_eq!(ctx.start, "/provider login ".len());
    }

    #[test]
    fn walker_returns_bounded_relative_paths() {
        let root = std::env::temp_dir().join(format!("bingo-complete-{}-walk", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/deep")).expect("mkdir");
        std::fs::create_dir_all(root.join("target")).expect("mkdir");
        std::fs::create_dir_all(root.join(".hidden")).expect("mkdir");
        std::fs::write(root.join("README.md"), "x").expect("write");
        std::fs::write(root.join("src/main.rs"), "x").expect("write");
        std::fs::write(root.join("src/deep/mod.rs"), "x").expect("write");
        std::fs::write(root.join("target/junk.o"), "x").expect("write");
        std::fs::write(root.join(".hidden/secret"), "x").expect("write");

        let (files, truncated) = walk_files(&root, 100);
        assert!(!truncated);
        assert_eq!(
            files,
            vec![
                "README.md".to_string(),
                "src/deep/mod.rs".to_string(),
                "src/main.rs".to_string(),
            ],
            "relative, /-joined, no target/ and no hidden dirs"
        );

        let (capped, truncated) = walk_files(&root, 2);
        assert!(truncated, "the cap is reported");
        assert_eq!(capped.len(), 2);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn project_files_uses_git_inside_a_repository() {
        // This worktree is a git repository, so the git source must answer and
        // must return paths relative to it.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let (files, _) = project_files(root, MENTION_FILE_CAP);
        assert!(
            files.iter().any(|f| f == "src/tui/complete.rs"),
            "git ls-files lists this very file relative to the repo root"
        );
        assert!(
            files.iter().all(|f| !f.starts_with('/')),
            "every entry is relative"
        );
    }
}
