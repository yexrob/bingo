//! Slack-shaped workspace view (D43): the skin the entity modal wears.
//!
//! The mapping is one-to-one with what the runtime already has, so nothing new
//! had to be invented to make the shape fit:
//!
//! | Slack            | bingo                                        |
//! |------------------|----------------------------------------------|
//! | workspace        | the team (`.bingo/team.json` name)           |
//! | channel `#name`  | [`crate::channels`] channel                  |
//! | direct message   | a subagent instance                          |
//! | app/bot messages | agent text replies                           |
//!
//! Everything here is a pure function of a [`Snapshot`] (what the session holds
//! right now) plus [`Workspace`] (where the eye is). The host loop in
//! [`crate::tui::entity`] owns the terminal and polls the snapshot each frame —
//! the row model itself stays renderer-agnostic and testable, exactly like the
//! rest of the display layer.
//!
//! D47 cut the rail and the sidebar: the conversation is the whole view, and
//! nothing here paints a surface. Rows carry foregrounds and let the terminal's
//! own background show through, so the only backgrounds left are marks — the
//! avatar chip, the switcher's selected row — plus the explicit erase [`opaque`]
//! needs to keep an overlay from being see-through.

use ratatui::layout::Rect;
use ratatui::style::Color;

use rsmarkdown_core::{MarkdownProcessor, Renderer};

use crate::agents::AgentState;
use crate::api::types::{ContentBlock, Message, Role};
use crate::channels::{ChannelMessage, ChannelMode};
use crate::tui::avatar;
use crate::tui::chat::Row;
use crate::tui::line::{Line, SegStyle, text_width, wrap_words};
use crate::tui::markdown::MarkdownRenderer;
use crate::tui::theme::Theme;

/// Left gutter of the message list when the avatar is a text chip: ` X ` plus one
/// space. With image avatars it is [`avatar::COLS`] plus one — see [`gutter`].
const GUTTER: usize = 4;
/// Consecutive messages from one sender inside this window are grouped under a
/// single name row (Slack groups on a 5-minute window).
const GROUP_WINDOW: u64 = 300;
/// Composer box grows to at most this many text rows before it scrolls.
const COMPOSER_MAX_ROWS: usize = 5;
/// Matches the quick switcher lists at once.
const SWITCHER_ROWS: usize = 8;

/// The conversation skin. Slack's *layout*, bingo's colours — and, since the rail
/// and sidebar were cut, no surface colours at all: the view paints foregrounds and
/// lets the terminal's own background show through. The only backgrounds left are
/// marks that mean something (the avatar chip, the switcher's selected row), never
/// chrome.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub badge_bg: Color,
    pub badge_fg: Color,
    pub presence_on: Color,
    pub presence_off: Color,
    pub main_text: Color,
    pub main_dim: Color,
    pub divider: Color,
    pub accent: Color,
    pub unread: Color,
    pub avatars: [Color; 6],
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    /// Accents come from the terminal theme, so the workspace moves with the rest
    /// of the app instead of pinning a second brand on top of it.
    pub fn new(theme: &Theme) -> Self {
        let base = Palette {
            badge_bg: theme.claude_deep_strong,
            badge_fg: rgb(0xFFFFFF),
            presence_on: theme.success,
            presence_off: rgb(0x776C62),
            main_text: theme.text,
            main_dim: theme.inactive,
            divider: rgb(0x38332D),
            accent: theme.claude,
            unread: theme.claude_strong,
            avatars: [
                rgb(0x4C9AE0),
                rgb(0x3FA96B),
                rgb(0xC9922E),
                rgb(0xCB5A74),
                rgb(0x7C6BD0),
                rgb(0xC1743C),
            ],
        };
        let pal = if theme.is_dark {
            base
        } else {
            Palette {
                main_text: rgb(0x1D1C1D),
                main_dim: rgb(0x616061),
                divider: rgb(0xDDDDDD),
                accent: theme.claude_deep,
                ..base
            }
        };
        if Theme::terminal_supports_truecolor() {
            pal
        } else {
            pal.downgrade_to_256()
        }
    }

    /// Terminals without 24-bit colour ignore RGB sequences outright, so the
    /// whole skin has to come down to the 256-colour cube together.
    fn downgrade_to_256(self) -> Self {
        let f = crate::tui::theme::to_ansi256;
        Palette {
            badge_bg: f(self.badge_bg),
            badge_fg: f(self.badge_fg),
            presence_on: f(self.presence_on),
            presence_off: f(self.presence_off),
            main_text: f(self.main_text),
            main_dim: f(self.main_dim),
            divider: f(self.divider),
            accent: f(self.accent),
            unread: f(self.unread),
            avatars: self.avatars.map(f),
        }
    }
}

/// Which conversation is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conv {
    Channel(String),
    Dm(String),
}

impl Conv {
    pub fn name(&self) -> &str {
        match self {
            Conv::Channel(n) | Conv::Dm(n) => n,
        }
    }
}

/// One channel as the view sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelItem {
    pub name: String,
    pub seq: u64,
    pub unread: u64,
    pub frozen: bool,
    pub mode: ChannelMode,
    pub members: Vec<String>,
}

/// One subagent instance as the view sees it (a DM correspondent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmItem {
    pub name: String,
    pub state: AgentState,
    pub model: String,
    pub thinking: Option<String>,
    pub description: String,
    pub unread: u64,
}

/// Everything the view needs from the session, sampled once per frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub workspace: String,
    pub main_model: String,
    pub main_thinking: Option<String>,
    pub channels: Vec<ChannelItem>,
    pub dms: Vec<DmItem>,
}

impl Snapshot {
    /// Every conversation, channels first (sidebar order).
    pub fn all(&self) -> Vec<Conv> {
        self.channels
            .iter()
            .map(|c| Conv::Channel(c.name.clone()))
            .chain(self.dms.iter().map(|d| Conv::Dm(d.name.clone())))
            .collect()
    }

    fn channel(&self, name: &str) -> Option<&ChannelItem> {
        self.channels.iter().find(|c| c.name == name)
    }

    fn dm(&self, name: &str) -> Option<&DmItem> {
        self.dms.iter().find(|d| d.name == name)
    }

    fn unread_of(&self, conv: &Conv) -> u64 {
        match conv {
            Conv::Channel(n) => self.channel(n).map(|c| c.unread).unwrap_or(0),
            Conv::Dm(n) => self.dm(n).map(|d| d.unread).unwrap_or(0),
        }
    }
}

/// What a message row shows besides its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostKind {
    /// An ordinary message.
    Said,
    /// Sent but still in the inbox — delivery happens at the next turn boundary.
    Queued,
    /// The streaming tail of a running turn (Slack's "…is typing").
    Typing,
    /// Wake-up scaffolding the runtime wrote into the instance's history — a
    /// relayed channel message, a follow-up chase, the task reminder. Nobody
    /// typed it, so it gets one dim line instead of a quoted block with a name
    /// and an avatar over it.
    Note,
}

/// One rendered message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub from: String,
    /// Written by the human sitting in front of the terminal.
    pub you: bool,
    /// Unix seconds; 0 when the source carries no clock.
    pub at: u64,
    pub text: String,
    pub kind: PostKind,
}

/// Channel log → posts.
pub fn channel_posts(log: &[ChannelMessage], me: &str) -> Vec<Post> {
    log.iter()
        .map(|m| Post {
            from: m.from.clone(),
            you: m.from == me,
            at: m.at,
            text: m.text.clone(),
            kind: PostKind::Said,
        })
        .collect()
}

/// Subagent history → posts. User turns and assistant text form the DM;
/// tool activity, tool results, and thinking stay out. The main transcript is
/// where execution detail lives.
/// One line of runtime scaffolding → how it reads collapsed, or `None` for text
/// a person actually wrote. The shapes are the ones `absorb_inbox` and the task
/// reminder compose, so this stays in step with them by construction: anything
/// the runtime wraps in its own brackets is not a message from the user.
fn scaffold_note(line: &str) -> Option<String> {
    let line = line.trim_end();
    if let Some(rest) = line.strip_prefix("[#")
        && let Some((head, body)) = rest.split_once("] ")
        && let Some((channel, _)) = head.split_once(' ')
    {
        return Some(format!("#{channel} · {body}"));
    }
    if line.starts_with("[follow-up ") {
        return Some("follow-up · waiting for a reply".to_string());
    }
    None
}

/// A user-role message → posts. Runtime scaffolding collapses to [`PostKind::Note`]
/// lines; whatever a person actually wrote stays a message.
fn user_posts(text: &str, me: &str) -> Vec<Post> {
    let mut out = Vec::new();
    let mut plain: Vec<&str> = Vec::new();
    let flush = |plain: &mut Vec<&str>, out: &mut Vec<Post>| {
        let joined = plain.join("\n");
        plain.clear();
        if joined.trim().is_empty() {
            return;
        }
        out.push(Post {
            from: me.to_string(),
            you: true,
            at: 0,
            text: joined,
            kind: PostKind::Said,
        });
    };
    for line in text.lines() {
        // The task reminder is a block, not a line: its marker heads a paragraph
        // of instructions and the current task list, and everything after it in
        // this message belongs to it.
        if line.starts_with(crate::query::TASK_REMINDER_MARKER) {
            flush(&mut plain, &mut out);
            out.push(Post {
                from: me.to_string(),
                you: true,
                at: 0,
                text: "system note · task tools".to_string(),
                kind: PostKind::Note,
            });
            return out;
        }
        match scaffold_note(line) {
            Some(note) => {
                flush(&mut plain, &mut out);
                out.push(Post {
                    from: me.to_string(),
                    you: true,
                    at: 0,
                    text: note,
                    kind: PostKind::Note,
                });
            }
            None => plain.push(line),
        }
    }
    flush(&mut plain, &mut out);
    out
}

pub fn dm_posts(
    history: &[Message],
    live: &[crate::agents::LiveBlock],
    pending: &[String],
    who: &str,
    me: &str,
) -> Vec<Post> {
    let mut out = Vec::new();
    for msg in history {
        for block in &msg.content {
            match (msg.role, block) {
                (Role::User, ContentBlock::Text { text }) => out.extend(user_posts(text, me)),
                (Role::Assistant, ContentBlock::Text { text }) => out.push(Post {
                    from: who.to_string(),
                    you: false,
                    at: 0,
                    text: text.clone(),
                    kind: PostKind::Said,
                }),
                _ => {}
            }
        }
    }
    for text in pending {
        out.push(Post {
            from: me.to_string(),
            you: true,
            at: 0,
            text: text.clone(),
            kind: PostKind::Queued,
        });
    }
    // Only prose belongs in the DM. A tool block still keeps the trailing
    // working indicator alive below, so silent waits remain visible without
    // exposing execution detail.
    let typing_at = match live.last() {
        Some(crate::agents::LiveBlock::Text(t)) if !t.trim().is_empty() => Some(live.len() - 1),
        _ => None,
    };
    for (i, block) in live.iter().enumerate() {
        match block {
            crate::agents::LiveBlock::Text(text) if !text.trim().is_empty() => out.push(Post {
                from: who.to_string(),
                you: false,
                at: 0,
                text: text.clone(),
                kind: if Some(i) == typing_at {
                    PostKind::Typing
                } else {
                    PostKind::Said
                },
            }),
            crate::agents::LiveBlock::Tool(_) | crate::agents::LiveBlock::Text(_) => {}
        }
    }
    // Mid-tool or between rounds, the agent is still working. Keep this feedback
    // even though the tool itself is hidden: otherwise the DM goes silent during
    // exactly the wait the user cannot infer from visible prose.
    if typing_at.is_none() && !live.is_empty() {
        out.push(Post {
            from: who.to_string(),
            you: false,
            at: 0,
            text: String::new(),
            kind: PostKind::Typing,
        });
    }
    out
}

/// What the keyboard is aimed at. With the sidebar gone there are two places to
/// be: reading the transcript, or writing into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Messages,
    Composer,
}

impl Focus {
    pub fn next(self) -> Focus {
        match self {
            Focus::Messages => Focus::Composer,
            Focus::Composer => Focus::Messages,
        }
    }
}

/// The ctrl+K quick switcher.
#[derive(Debug, Clone, Default)]
pub struct Switcher {
    pub query: String,
    pub sel: usize,
}

/// Where the eye is. Held by the host loop across frames; every renderer here
/// reads it and never writes it.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub open: Option<Conv>,
    /// Selection index into the conversation list (what alt+↑/↓ walks).
    pub sel: usize,
    pub focus: Focus,
    /// Rows scrolled up from the bottom (0 = pinned to the latest).
    pub scroll_up: usize,
    pub composer: String,
    pub switcher: Option<Switcher>,
    pub flash: Option<String>,
    /// Read cursor per conversation, advanced while it is on screen.
    pub read: Vec<(Conv, u64)>,
    /// Read cursor captured the moment the open conversation was entered. The
    /// unread divider stays put on it while you read, the way Slack keeps the
    /// line where you left off until you leave the channel.
    pub entered: Option<(Conv, u64)>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            open: None,
            sel: 0,
            focus: Focus::Composer,
            scroll_up: 0,
            composer: String::new(),
            switcher: None,
            flash: None,
            read: Vec::new(),
            entered: None,
        }
    }
}

impl Workspace {
    /// Remember how far the user has read in a conversation (the unread divider
    /// and the sidebar badge both hang off this).
    pub fn mark_read(&mut self, conv: &Conv, seq: u64) {
        match self.read.iter_mut().find(|(c, _)| c == conv) {
            Some((_, cursor)) => *cursor = (*cursor).max(seq),
            None => self.read.push((conv.clone(), seq)),
        }
    }

    pub fn read_cursor(&self, conv: &Conv) -> u64 {
        self.read
            .iter()
            .find(|(c, _)| c == conv)
            .map(|(_, seq)| *seq)
            .unwrap_or(0)
    }

    pub fn knows(&self, conv: &Conv) -> bool {
        self.read.iter().any(|(c, _)| c == conv)
    }

    /// Where the unread divider sits in the open conversation.
    pub fn entry_cursor(&self, conv: &Conv) -> u64 {
        match &self.entered {
            Some((c, seq)) if c == conv => *seq,
            _ => self.read_cursor(conv),
        }
    }

    /// Step to the next/previous conversation and open it (Slack's alt+↑/↓).
    /// With no sidebar this and the ctrl+K switcher are the whole of navigation,
    /// so it walks every conversation there is, channels before DMs.
    pub fn step(&mut self, snap: &Snapshot, delta: isize) {
        let convs = snap.all();
        if convs.is_empty() {
            return;
        }
        let last = convs.len() - 1;
        self.sel = match delta {
            d if d < 0 => self.sel.saturating_sub(d.unsigned_abs()),
            d => (self.sel + d as usize).min(last),
        };
        self.select(convs[self.sel].clone());
    }

    /// Open a conversation: resets the scroll, clears any stale notice, and
    /// pins the unread divider where reading started.
    pub fn select(&mut self, conv: Conv) {
        if self.open.as_ref() != Some(&conv) {
            self.scroll_up = 0;
            self.flash = None;
            self.entered = Some((conv.clone(), self.read_cursor(&conv)));
        }
        self.open = Some(conv);
    }

    /// Re-seat the selection after the conversation list changed underneath it.
    /// A conversation that vanished (instance deleted, channel gone) hands the
    /// cursor to the first one still standing rather than dangling.
    pub fn sync(&mut self, snap: &Snapshot) {
        let all = snap.all();
        if all.is_empty() {
            self.open = None;
            self.sel = 0;
            return;
        }
        if self.open.as_ref().is_none_or(|o| !all.contains(o)) {
            self.open = all.first().cloned();
        }
        self.sel = self
            .open
            .as_ref()
            .and_then(|o| all.iter().position(|c| c == o))
            .unwrap_or(0);
    }
}

/// The conversation gets the whole terminal. The rail and sidebar this view used
/// to carry were cut: navigation lives in the ctrl+K switcher and alt+↑/↓, and
/// two columns of chrome are two columns not spent on the conversation.
pub fn layout(area: Rect) -> Rect {
    area
}

fn row(line: Line) -> Row {
    Row::new(line)
}

fn blank() -> Row {
    row(Line::empty())
}

/// An empty conversation row, for padding a short message list down to the
/// composer.
pub fn blank_row() -> Row {
    blank()
}

/// A row that erases the full width before it draws. `Color::Reset` is not a
/// colour the view chose — it is the terminal's own background — but it has to be
/// stated: ratatui *patches* styles, so a span carrying no background leaves the
/// cell's old one in place, and an overlay row of spaces would let whatever it
/// covers (an avatar chip, most visibly) glow through from underneath.
fn opaque(line: Line) -> Row {
    let mut r = Row::new(line);
    r.bg = Some(Color::Reset);
    r
}

/// Pad rows out to `height` so what follows them lands where it was measured to
/// land.
fn pad(mut rows: Vec<Row>, height: usize) -> Vec<Row> {
    rows.truncate(height);
    while rows.len() < height {
        rows.push(blank());
    }
    rows
}

/// Centre `text` in a `width`-cell box, padded on both sides so a background
/// colour fills the whole column instead of hugging the glyph.
fn boxed(text: &str, width: usize) -> String {
    let w = text_width(text);
    let lead = width.saturating_sub(w).div_ceil(2);
    let tail = width.saturating_sub(w + lead);
    format!("{}{text}{}", " ".repeat(lead), " ".repeat(tail))
}

/// Local wall-clock `HH:MM`; empty when the source carries no timestamp.
fn hhmm(at: u64) -> String {
    use chrono::TimeZone;
    if at == 0 {
        return String::new();
    }
    chrono::Local
        .timestamp_opt(at as i64, 0)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// Day-divider label: Today / Yesterday / M/D. `None` when there is no clock to
/// divide by.
fn day_label(at: u64) -> Option<String> {
    use chrono::TimeZone;
    if at == 0 {
        return None;
    }
    let t = chrono::Local.timestamp_opt(at as i64, 0).single()?;
    let today = chrono::Local::now().date_naive();
    let day = t.date_naive();
    let delta = (today - day).num_days();
    Some(match delta {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        _ => t.format("%-m/%-d").to_string(),
    })
}

fn presence(state: AgentState, pal: &Palette) -> (&'static str, Color) {
    match state {
        AgentState::Running => ("●", pal.presence_on),
        AgentState::Idle => ("○", pal.presence_off),
        AgentState::Stopped => ("·", pal.presence_off),
    }
}

fn member_engine(snap: &Snapshot, name: &str) -> Option<String> {
    if name == crate::channels::HUB_NAME {
        return Some(format!(
            "{name}: {}/{}",
            snap.main_model,
            snap.main_thinking.as_deref().unwrap_or("off")
        ));
    }
    let dm = snap.dm(name)?;
    Some(format!(
        "{name}: {}/{}",
        dm.model,
        dm.thinking.as_deref().unwrap_or("off")
    ))
}

fn channel_engine_meta(snap: &Snapshot, channel: &ChannelItem, width: usize) -> String {
    let engines = channel
        .members
        .iter()
        .filter_map(|member| member_engine(snap, member))
        .collect::<Vec<_>>();
    let named = engines.join(", ");
    if engines.len() <= 3 && text_width(&named) <= width {
        return named;
    }
    let summaries = engines
        .iter()
        .map(|engine| {
            engine
                .split_once(':')
                .map_or(engine.as_str(), |(_, engine)| engine.trim())
        })
        .collect::<Vec<_>>();
    if let Some(engine) = summaries.first()
        && summaries.iter().all(|candidate| candidate == engine)
    {
        return format!("{} agents · {engine}", summaries.len());
    }
    format!("{} agents · mixed engines", summaries.len())
}

/// Conversation header: who you are looking at, what it is, and a rule under it.
/// DM metadata trails the title; small channels get wrapped per-member engine
/// rows, while larger rooms get one bounded engine summary.
pub fn header_rows(snap: &Snapshot, conv: &Conv, pal: &Palette, width: usize) -> Vec<Row> {
    let engine_width = width.saturating_sub(3).max(1);
    let (title, meta, engines) = match conv {
        Conv::Channel(name) => match snap.channel(name) {
            Some(c) => (
                format!(" # {name}"),
                format!(
                    "  {} · {} msgs · {} people{}",
                    c.mode.label(),
                    c.seq,
                    c.members.len(),
                    if c.frozen { " · frozen" } else { "" }
                ),
                channel_engine_meta(snap, c, engine_width),
            ),
            None => (
                format!(" # {name}"),
                "  no longer exists".to_string(),
                String::new(),
            ),
        },
        Conv::Dm(name) => match snap.dm(name) {
            Some(d) => {
                let (glyph, _) = presence(d.state, pal);
                (
                    format!(" {glyph} {name}"),
                    format!(
                        "  DM · {} · {} · {} · {}",
                        d.model,
                        d.thinking.as_deref().unwrap_or("off"),
                        d.state.label(),
                        d.description
                    ),
                    String::new(),
                )
            }
            None => (
                format!(" {name}"),
                "  no longer exists".to_string(),
                String::new(),
            ),
        },
    };
    // The workspace name sits right, where the member count used to: it is the one
    // piece of context the cut sidebar was carrying that nothing else says.
    let right = format!("{} ", snap.workspace);
    let room = width.saturating_sub(text_width(&right));
    let mut head = Line::styled(
        crate::tui::chat::one_line(&title, room),
        SegStyle::fg(pal.main_text).bold(),
    );
    let meta = crate::tui::chat::one_line(
        &meta,
        room.saturating_sub(text_width(&head.plain_text())).max(1),
    );
    head.push_styled(meta, SegStyle::fg(pal.main_dim));
    let gap = width
        .saturating_sub(text_width(&head.plain_text()))
        .saturating_sub(text_width(&right));
    head.push_styled(" ".repeat(gap), SegStyle::fg(pal.main_dim));
    head.push_styled(right, SegStyle::fg(pal.main_dim));
    let mut rows = vec![row(head)];
    if !engines.is_empty() {
        let engine = crate::tui::chat::one_line(&engines, engine_width);
        let mut line = Line::styled("  ", SegStyle::fg(pal.main_dim));
        line.push_styled(engine, SegStyle::fg(pal.main_dim));
        rows.push(row(line));
    }
    rows.push(row(Line::styled(
        "─".repeat(width.min(500)),
        SegStyle::fg(pal.divider),
    )));
    rows
}

/// Slack's day divider: a hairline rule with the day centred on it.
fn day_divider(label: &str, pal: &Palette, width: usize) -> Row {
    let text = format!(" {label} ");
    let w = text_width(&text);
    let side = width.saturating_sub(w) / 2;
    let mut line = Line::styled("─".repeat(side), SegStyle::fg(pal.divider));
    line.push_styled(text, SegStyle::fg(pal.main_dim));
    line.push_styled(
        "─".repeat(width.saturating_sub(side + w)),
        SegStyle::fg(pal.divider),
    );
    row(line)
}

/// Slack's unread marker: the rule itself goes red and the label sits at the
/// right end, so the eye catches the line before it reads the words.
fn unread_divider(pal: &Palette, width: usize) -> Row {
    let label = " new messages ";
    let w = text_width(label);
    let rule = width.saturating_sub(w).saturating_sub(1);
    let mut line = Line::styled("─".repeat(rule), SegStyle::fg(pal.unread));
    line.push_styled(label, SegStyle::fg(pal.unread).bold());
    row(line)
}

/// How a sender's face is chosen: whether the terminal can place a portrait at
/// all, plus the ones the blueprint pinned. A crew is a standing cast, so its
/// faces come from `.bingo/team.json` rather than from hashing an instance name —
/// which collides often across four role names, and changes the moment a member
/// is renamed. `default()` is the text skin: no portraits, no pins.
#[derive(Debug, Clone, Default)]
pub struct Avatars {
    /// The terminal can place kitty images.
    pub images: bool,
    /// Instance name → portrait index, from the team blueprint.
    pub pinned: std::collections::HashMap<String, usize>,
}

impl Avatars {
    /// The portrait a sender wears — pinned if the blueprint says so, otherwise
    /// derived from the name so a passing subagent still gets a stable face.
    pub fn index_of(&self, sender: &str) -> usize {
        self.pinned
            .get(sender)
            .copied()
            .unwrap_or_else(|| avatar::index_of(sender))
    }
}

/// Message gutter: wide enough for whichever avatar the terminal can draw.
fn gutter(images: bool) -> usize {
    if images { avatar::COLS + 1 } else { GUTTER }
}

/// Avatar chip for terminals that cannot place images: the sender's initial on a
/// colour, occupying the same gutter the portrait would. The colour is keyed to
/// the same portrait index, so a pinned member keeps one identity in both skins.
fn chip(name: &str, index: usize, pal: &Palette) -> Line {
    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "·".to_string());
    let cell = if text_width(&initial) > 1 {
        initial
    } else {
        format!("{initial} ")
    };
    Line::styled(
        format!(" {cell}"),
        SegStyle::fg(pal.badge_fg)
            .with_bg(pal.avatars[index % pal.avatars.len()])
            .bold(),
    )
}

/// The `row`-th gutter cell of a message block: the avatar's own rows when the
/// terminal can place images, blank indentation otherwise. Row 0 rides the name
/// line, row 1 the first body line — which is why the portrait costs no rows the
/// layout was not already spending.
fn gutter_line(name: &str, row: usize, avatars: &Avatars, pal: &Palette) -> Line {
    gutter_cell(avatars.index_of(name), name, row, avatars.images, pal)
}

/// [`gutter_line`] with the portrait already resolved: the main transcript knows
/// the index before it knows the name (the hub and the human are not blueprint
/// members), so it enters here rather than through an [`Avatars`] table it would
/// have to clone every frame.
pub fn gutter_cell(index: usize, name: &str, row: usize, images: bool, pal: &Palette) -> Line {
    if images {
        if let Some((cells, id)) = avatar::placeholder(index, row) {
            let mut line = Line::styled(cells, SegStyle::fg(id));
            line.push_styled(" ", SegStyle::fg(pal.main_text));
            return line;
        }
    } else if row == 0 {
        let mut line = chip(name, index, pal);
        line.push_styled(" ", SegStyle::fg(pal.main_text));
        return line;
    }
    Line::styled(" ".repeat(gutter(images)), SegStyle::fg(pal.main_dim))
}

/// A sender band: the portrait beside the name, as the rows a transcript puts
/// *above* a message rather than beside it. The main chat has no gutter — its
/// bodies run the full width and its `⏺` markers separate prose from tool rows
/// inside one message — so the face goes overhead, where it costs two rows once
/// per message and nothing below it has to move.
///
/// One row where the terminal cannot place images: unlike the workspace gutter,
/// nothing here depends on the two skins having equal height.
pub fn sender_band(
    index: usize,
    name: &str,
    shown: &str,
    images: bool,
    pal: &Palette,
) -> Vec<Line> {
    let mut head = gutter_cell(index, name, 0, images, pal);
    head.push_styled(shown.to_string(), SegStyle::fg(pal.main_text).bold());
    if !images {
        return vec![head];
    }
    vec![head, gutter_cell(index, name, 1, images, pal)]
}

fn indent_rows(
    body: Vec<Row>,
    post: &Post,
    grouped: bool,
    gutter_w: usize,
    avatars: &Avatars,
    pal: &Palette,
) -> Vec<Row> {
    body.into_iter()
        .enumerate()
        .map(|(i, body)| {
            let mut line = if i == 0 && !grouped {
                gutter_line(&post.from, 1, avatars, pal)
            } else {
                Line::styled(" ".repeat(gutter_w), SegStyle::fg(pal.main_dim))
            };
            let gutter_segments = line.segs.len();
            line.image = body.line.image.clone();
            line.segs.extend(body.line.segs);
            let bg = body.bg;
            if let Some(bg) = bg {
                for seg in line.segs.iter_mut().skip(gutter_segments) {
                    seg.style.bg = Some(bg);
                }
            }
            let mut row = Row::new(line);
            row.bg = None;
            row.padding_right = body.padding_right;
            row
        })
        .collect()
}

/// The DM body uses the same user bubble and assistant markdown row builders as
/// the main transcript. The surrounding name row and avatar gutter remain the
/// workspace's existing DM skin.
fn dm_body_rows(post: &Post, width: usize, theme: &Theme) -> Vec<Row> {
    match post.kind {
        PostKind::Queued => wrap_words(&post.text, width)
            .into_iter()
            .map(|line| Row::new(Line::styled(line, SegStyle::fg(theme.inactive))))
            .collect(),
        PostKind::Typing if post.text.is_empty() => Vec::new(),
        _ if post.you => crate::tui::chat::user_message_rows(&post.text, width, theme),
        _ => {
            let mut processor = MarkdownProcessor::default();
            let mut renderer = MarkdownRenderer::with_theme(width.saturating_sub(2), theme.clone());
            let doc = processor.process_streaming(&post.text);
            renderer.render(&doc);
            crate::tui::chat::text_rows(theme, renderer.lines().to_vec())
        }
    }
}

fn workspace_body_rows(post: &Post, width: usize, pal: &Palette) -> Vec<Row> {
    let style = match post.kind {
        PostKind::Queued => SegStyle::fg(pal.main_dim),
        _ => SegStyle::fg(pal.main_text),
    };
    let mut rows = Vec::new();
    for para in post.text.lines() {
        let wrapped = wrap_words(para, width);
        if wrapped.is_empty() {
            rows.push(Row::new(Line::empty()));
        }
        rows.extend(
            wrapped
                .into_iter()
                .map(|line| Row::new(Line::styled(line, style))),
        );
    }
    rows
}

/// The message list. Consecutive posts from one sender inside [`GROUP_WINDOW`]
/// share a name row; the day changes and the first unread message get dividers.
/// `avatars` says whether the terminal can place portraits and which ones the
/// blueprint pinned; the row count is the same either way, so only the gutter
/// changes.
pub fn message_rows(
    posts: &[Post],
    unread_from: usize,
    pal: &Palette,
    width: usize,
    avatars: &Avatars,
) -> Vec<Row> {
    message_rows_for(posts, unread_from, pal, width, avatars, None)
}

pub fn dm_message_rows(
    posts: &[Post],
    unread_from: usize,
    pal: &Palette,
    width: usize,
    avatars: &Avatars,
    theme: &Theme,
) -> Vec<Row> {
    message_rows_for(posts, unread_from, pal, width, avatars, Some(theme))
}

fn message_rows_for(
    posts: &[Post],
    unread_from: usize,
    pal: &Palette,
    width: usize,
    avatars: &Avatars,
    main_theme: Option<&Theme>,
) -> Vec<Row> {
    if posts.is_empty() {
        return vec![
            blank(),
            row(Line::styled(
                "   No messages yet. Write the first one below.",
                SegStyle::fg(pal.main_dim).italic(),
            )),
        ];
    }
    let gutter_w = gutter(avatars.images);
    let body_w = width.saturating_sub(gutter_w + 1).max(8);
    let mut rows: Vec<Row> = Vec::new();
    let mut prev: Option<(&str, u64)> = None;
    let mut prev_day: Option<String> = None;
    for (i, post) in posts.iter().enumerate() {
        if let Some(label) = day_label(post.at)
            && prev_day.as_deref() != Some(label.as_str())
        {
            rows.push(blank());
            rows.push(day_divider(&label, pal, width));
            prev_day = Some(label);
            prev = None;
        }
        if i == unread_from && unread_from < posts.len() {
            rows.push(blank());
            rows.push(unread_divider(pal, width));
            prev = None;
        }
        // Scaffolding belongs to nobody: one dim line, no name row, no avatar,
        // and it does not break the grouping of the messages around it.
        if post.kind == PostKind::Note {
            let mut line = Line::styled(" ".repeat(gutter_w), SegStyle::fg(pal.main_dim));
            line.push_styled("▏", SegStyle::fg(pal.divider));
            line.push_styled(
                format!(" {}", crate::tui::chat::one_line(&post.text, body_w)),
                SegStyle::fg(pal.main_dim).italic(),
            );
            rows.push(row(line));
            continue;
        }
        let grouped = prev.is_some_and(|(from, at)| {
            from == post.from
                && post.kind != PostKind::Typing
                && (post.at == 0 || at == 0 || post.at.saturating_sub(at) <= GROUP_WINDOW)
        });
        if !grouped {
            rows.push(blank());
            let mut head = gutter_line(&post.from, 0, avatars, pal);
            let shown = if post.you {
                "You".to_string()
            } else {
                post.from.clone()
            };
            head.push_styled(shown, SegStyle::fg(pal.main_text).bold());
            let time = hhmm(post.at);
            if !time.is_empty() {
                head.push_styled(format!("  {time}"), SegStyle::fg(pal.main_dim));
            }
            rows.push(row(head));
        }
        let body = match main_theme {
            Some(theme) => dm_body_rows(post, body_w, theme),
            None => workspace_body_rows(post, body_w, pal),
        };
        let mut body = indent_rows(body, post, grouped, gutter_w, avatars, pal);
        let mut lead = body.len();
        rows.append(&mut body);
        if post.kind == PostKind::Queued {
            let mut line = if lead == 0 && !grouped {
                gutter_line(&post.from, 1, avatars, pal)
            } else {
                Line::styled(" ".repeat(gutter_w), SegStyle::fg(pal.main_dim))
            };
            lead += 1;
            line.push_styled(
                "⧖ pending delivery (injected at the next turn boundary)",
                SegStyle::fg(pal.main_dim).italic(),
            );
            rows.push(row(line));
        }
        if post.kind == PostKind::Typing {
            let mut line = if lead == 0 && !grouped {
                gutter_line(&post.from, 1, avatars, pal)
            } else {
                Line::styled(" ".repeat(gutter_w), SegStyle::fg(pal.main_dim))
            };
            lead += 1;
            line.push_styled(
                format!("✻ {} is typing…", post.from),
                SegStyle::fg(pal.accent).italic(),
            );
            rows.push(row(line));
        }
        // A post with no body at all still owes the portrait its second row.
        if lead == 0 && !grouped {
            rows.push(row(gutter_line(&post.from, 1, avatars, pal)));
        }
        prev = Some((&post.from, post.at));
    }
    rows
}

/// Composer box: a rounded frame, the draft inside it, and the key hints under
/// it. Returns the rows plus the caret cell, relative to the block.
pub fn composer_rows(
    ws: &Workspace,
    conv: &Conv,
    pal: &Palette,
    width: usize,
) -> (Vec<Row>, Option<(usize, usize)>) {
    // Box geometry: margin + │ + space + inner + space + │ == width.
    let inner = width.saturating_sub(5).max(4);
    let placeholder = match conv {
        Conv::Channel(name) => format!("message #{name}"),
        Conv::Dm(name) => format!("message {name}"),
    };
    let empty = ws.composer.is_empty();
    let text: Vec<String> = if empty {
        vec![placeholder]
    } else {
        let mut w = wrap_words(&ws.composer, inner);
        if w.is_empty() {
            w.push(String::new());
        }
        w
    };
    let shown: Vec<String> = text
        .iter()
        .skip(text.len().saturating_sub(COMPOSER_MAX_ROWS))
        .cloned()
        .collect();
    let active = ws.focus == Focus::Composer;
    let border = if active { pal.accent } else { pal.divider };
    let frame = |left: &str, right: &str| {
        row(Line::styled(
            format!(" {left}{}{right}", "─".repeat(inner + 2)),
            SegStyle::fg(border),
        ))
    };
    let mut rows = vec![frame("╭", "╮")];
    let mut caret = None;
    for (i, l) in shown.iter().enumerate() {
        let style = if empty {
            SegStyle::fg(pal.main_dim).italic()
        } else {
            SegStyle::fg(pal.main_text)
        };
        let mut line = Line::styled(" │ ", SegStyle::fg(border));
        line.push_styled(l.clone(), style);
        let used = text_width(l);
        line.push_styled(
            " ".repeat(inner.saturating_sub(used) + 1),
            SegStyle::fg(pal.main_text),
        );
        line.push_styled("│", SegStyle::fg(border));
        if i + 1 == shown.len() {
            caret = Some((rows.len(), if empty { 3 } else { 3 + used.min(inner) }));
        }
        rows.push(row(line));
    }
    rows.push(frame("╰", "╯"));
    let hint = if active {
        "  enter sends · tab switches focus · ctrl+k switches conversation · alt+↑↓ moves up/down · esc back"
    } else {
        "  tab returns to the composer · ↑↓ scrolls · ctrl+k switches conversation · alt+↑↓ moves up/down · esc back"
    };
    let foot = Line::styled(
        crate::tui::chat::one_line(hint, width.saturating_sub(2)),
        SegStyle::fg(pal.main_dim),
    );
    rows.push(row(foot));
    if let Some(flash) = &ws.flash {
        rows.push(row(Line::styled(
            crate::tui::chat::one_line(&format!("  ⚠ {flash}"), width),
            SegStyle::fg(pal.unread),
        )));
    }
    (rows, caret)
}

/// What the view shows when there is nothing to open yet.
pub fn empty_pane_rows(pal: &Palette, width: usize, height: usize) -> Vec<Row> {
    let lines = [
        ("No conversations here yet", true),
        ("", false),
        (
            "Instances spawned by the Agent tool become DM entries,",
            false,
        ),
        (
            "and rooms created by the Channel tool become channels.",
            false,
        ),
        ("", false),
        ("ctrl+k switches conversation · esc back", false),
    ];
    let top = height.saturating_sub(lines.len()) / 2;
    let mut rows = vec![blank(); top];
    for (text, strong) in lines {
        let style = if strong {
            SegStyle::fg(pal.main_text).bold()
        } else {
            SegStyle::fg(pal.main_dim)
        };
        rows.push(row(Line::styled(boxed(text, width), style)));
    }
    pad(rows, height)
}

/// Quick-switcher overlay rows and the matches they list (ctrl+K). With the
/// sidebar gone this is the main way to change conversation, so it opens on the
/// full list rather than waiting for a query.
pub fn switcher_rows(
    snap: &Snapshot,
    sw: &Switcher,
    pal: &Palette,
    width: usize,
) -> (Vec<Row>, Vec<Conv>) {
    let matches = switcher_matches(snap, &sw.query);
    // Same box geometry as the composer: margin + │ + space + inner + space + │.
    let inner = width.saturating_sub(5).max(8);
    // The overlay sits on top of the message list, so every row is padded out to
    // the full width *and* marked opaque: spaces erase the glyphs underneath,
    // `opaque` erases their colours.
    let boxed_row = |content: Line, used: usize| {
        let mut line = Line::styled(" │ ", SegStyle::fg(pal.accent));
        line.segs.extend(content.segs);
        line.push_styled(
            " ".repeat(inner.saturating_sub(used) + 1),
            SegStyle::fg(pal.main_text),
        );
        line.push_styled("│", SegStyle::fg(pal.accent));
        opaque(line)
    };
    // Margin + ╭ + inner+2 + ╮ == width, so the rule covers the same span as
    // every other overlay row.
    let rule = |left: &str, right: &str| {
        opaque(Line::styled(
            format!(" {left}{}{right}", "─".repeat(inner + 2)),
            SegStyle::fg(pal.accent),
        ))
    };

    let mut rows = vec![rule("╭", "╮")];
    let query_text = crate::tui::chat::one_line(&sw.query, inner.saturating_sub(5));
    let mut query = Line::styled("jump ", SegStyle::fg(pal.main_dim));
    query.push_styled(query_text.clone(), SegStyle::fg(pal.main_text));
    rows.push(boxed_row(query, 5 + text_width(&query_text)));

    if matches.is_empty() {
        let text = "no matching conversation";
        rows.push(boxed_row(
            Line::styled(text, SegStyle::fg(pal.main_dim).italic()),
            text_width(text),
        ));
    }
    for (i, conv) in matches.iter().enumerate().take(SWITCHER_ROWS) {
        let sel = i == sw.sel.min(matches.len().saturating_sub(1));
        let label = match conv {
            Conv::Channel(n) => format!("# {n}"),
            Conv::Dm(n) => format!("@ {n}"),
        };
        // The unread count the sidebar badge used to carry. This list is now the
        // only place the whole workspace is visible at once, so it is the only
        // place that can say where something new arrived.
        let unread = snap.unread_of(conv);
        let badge = if unread > 0 {
            format!(" {unread} ")
        } else {
            String::new()
        };
        let label = crate::tui::chat::one_line(&label, inner.saturating_sub(text_width(&badge)));
        let style = if sel {
            SegStyle::fg(pal.badge_fg).with_bg(pal.badge_bg).bold()
        } else {
            SegStyle::fg(pal.main_text)
        };
        // The selection bar runs the width of the row, Slack-style, so pad
        // inside the highlighted segment rather than after it.
        let used = text_width(&label) + text_width(&badge);
        let gap = inner.saturating_sub(used);
        let mut line = if sel {
            Line::styled(format!("{label}{}", " ".repeat(gap)), style)
        } else {
            let mut l = Line::styled(label, style);
            l.push_styled(" ".repeat(gap), SegStyle::fg(pal.main_dim));
            l
        };
        if !badge.is_empty() {
            line.push_styled(badge, SegStyle::fg(pal.unread).bold());
        }
        rows.push(boxed_row(line, inner));
    }
    rows.push(rule("╰", "╯"));
    (rows, matches)
}

/// Quick-switcher matching: case-insensitive substring, channels before DMs.
pub fn switcher_matches(snap: &Snapshot, query: &str) -> Vec<Conv> {
    let q = query.trim().to_lowercase();
    let q = q.trim_start_matches(['#', '@']);
    snap.channels
        .iter()
        .map(|c| Conv::Channel(c.name.clone()))
        .chain(snap.dms.iter().map(|d| Conv::Dm(d.name.clone())))
        .filter(|c| q.is_empty() || c.name().to_lowercase().contains(q))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pal() -> Palette {
        Palette::new(&Theme::dark())
    }

    fn snap() -> Snapshot {
        Snapshot {
            workspace: "bingo".into(),
            main_model: "main-model".into(),
            main_thinking: Some("high".into()),
            channels: vec![
                ChannelItem {
                    name: "dev-room".into(),
                    seq: 4,
                    unread: 2,
                    frozen: false,
                    mode: ChannelMode::Serial,
                    members: vec!["main".into(), "user".into(), "scout".into()],
                },
                ChannelItem {
                    name: "design".into(),
                    seq: 0,
                    unread: 0,
                    frozen: true,
                    mode: ChannelMode::Free,
                    members: vec!["main".into()],
                },
            ],
            dms: vec![
                DmItem {
                    name: "scout".into(),
                    state: AgentState::Running,
                    model: "gpt-5.6-sol".into(),
                    thinking: Some("max".into()),
                    description: "recon".into(),
                    unread: 0,
                },
                DmItem {
                    name: "qa".into(),
                    state: AgentState::Idle,
                    model: "claude-sonnet".into(),
                    thinking: Some("high".into()),
                    description: "acceptance".into(),
                    unread: 3,
                },
            ],
        }
    }

    fn texts(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| r.line.plain_text()).collect()
    }

    /// Emoji and other pictographs: a terminal picks whichever glyph its font
    /// carries, which is how a house icon turns into a colour emoji two cells
    /// wide. The chrome is built from text and box-drawing only.
    fn is_pictograph(c: char) -> bool {
        matches!(c as u32,
            0x1F300..=0x1FAFF   // emoji blocks
            | 0x2190..=0x21FF   // arrows
            | 0x2300..=0x23FF   // misc technical (⌂ ⌕ ⏺)
            | 0x2600..=0x27BF   // misc symbols + dingbats (✉ ✻ ➤)
            | 0x2B00..=0x2BFF)
    }

    #[test]
    fn messages_group_by_sender_and_break_on_unread() {
        let base = 1_760_000_000u64;
        let posts = vec![
            Post {
                from: "scout".into(),
                you: false,
                at: base,
                text: "found the regression".into(),
                kind: PostKind::Said,
            },
            Post {
                from: "scout".into(),
                you: false,
                at: base + 30,
                text: "in term.rs".into(),
                kind: PostKind::Said,
            },
            Post {
                from: "user".into(),
                you: true,
                at: base + 900,
                text: "got it".into(),
                kind: PostKind::Said,
            },
        ];
        let rows = message_rows(&posts, 2, &pal(), 60, &Avatars::default());
        let t = texts(&rows);
        // The second scout message is grouped: one name row, not two.
        assert_eq!(t.iter().filter(|l| l.contains("scout")).count(), 1, "{t:?}");
        // Your own messages are labelled You.
        assert!(t.iter().any(|l| l.contains("You")), "{t:?}");
        // Unread divider sits before the third post.
        assert!(t.iter().any(|l| l.contains("new messages")), "{t:?}");
        // Bodies sit in the avatar gutter.
        assert!(
            t.iter().any(|l| l.starts_with("    found the regression")),
            "{t:?}"
        );
        // Empty log gets the invitation, not a blank pane.
        assert!(
            texts(&message_rows(&[], 0, &pal(), 60, &Avatars::default()))
                .iter()
                .any(|l| l.contains("No messages yet"))
        );
    }

    /// The runtime writes its own scaffolding into an instance's history — the
    /// relay of a channel message, the task reminder. Quoting those in full put a
    /// wall of English system text under a "You" name row, as if the user had typed
    /// it. They collapse to one dim line each, and what a person actually wrote
    /// stays a message.
    #[test]
    fn wake_up_scaffolding_collapses_to_one_line() {
        let relay = format!(
            "[#dev-team msg #1] main: hello everyone, the user is greeting the team.\n{}\nThe task tools haven't been used recently.",
            crate::query::TASK_REMINDER_MARKER
        );
        let history = vec![
            crate::api::types::Message::user_text(relay),
            crate::api::types::Message::user_text("one thing just for you\nsecond line"),
        ];
        let posts = dm_posts(&history, &[], &[], "devex", "user");
        let kinds: Vec<PostKind> = posts.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![PostKind::Note, PostKind::Note, PostKind::Said],
            "{posts:?}"
        );
        assert_eq!(
            posts[0].text,
            "#dev-team · main: hello everyone, the user is greeting the team."
        );
        assert_eq!(posts[1].text, "system note · task tools");
        assert_eq!(
            posts[2].text, "one thing just for you\nsecond line",
            "what a real person wrote stays verbatim"
        );

        // A note owns no name row and no avatar, and does not split the grouping
        // of the messages around it.
        let rows = message_rows(&posts, usize::MAX, &pal(), 60, &Avatars::default());
        let t = texts(&rows);
        // The avatar chip is the only segment carrying a background, so counting
        // them counts name rows: the two notes get none, the real message gets one.
        let name_rows = rows
            .iter()
            .filter(|r| r.line.segs.iter().any(|s| s.style.bg.is_some()))
            .count();
        assert_eq!(
            name_rows, 1,
            "system rows do not impersonate the user or take an avatar: {t:?}"
        );
        assert!(t.iter().any(|l| l.contains("▏ #dev-team")), "{t:?}");
        assert!(t.iter().any(|l| l.contains("▏ system note")), "{t:?}");
    }

    /// A crew is a standing cast, so the blueprint's pin wins over the name hash —
    /// in both skins, so a member's identity does not change with the terminal.
    #[test]
    fn the_blueprint_pins_a_face() {
        let mut pinned = std::collections::HashMap::new();
        let wanted = crate::tui::avatar::index_of_id("sora").unwrap_or_else(|| panic!("has sora"));
        pinned.insert("Linh".to_string(), wanted);
        let avatars = Avatars {
            images: true,
            pinned,
        };
        assert_eq!(avatars.index_of("Linh"), wanted, "pinned face");
        assert_eq!(
            avatars.index_of("passing subagent"),
            crate::tui::avatar::index_of("passing subagent"),
            "unpinned ones still resolve by name"
        );

        // The pin reaches the rendered gutter: the id colour is the pinned
        // portrait's, not the one the name would have chosen.
        let posts = vec![Post {
            from: "Linh".into(),
            you: false,
            at: 0,
            text: "here".into(),
            kind: PostKind::Said,
        }];
        let rows = message_rows(&posts, usize::MAX, &pal(), 60, &avatars);
        let head = rows
            .iter()
            .find(|r| r.line.plain_text().contains("Linh"))
            .unwrap_or_else(|| panic!("has a name row"));
        let (_, want_color) = crate::tui::avatar::placeholder(wanted, 0)
            .unwrap_or_else(|| panic!("has a placeholder row"));
        assert_eq!(
            head.line.segs.first().map(|s| s.style.fg),
            Some(Some(want_color))
        );
    }

    /// Chrome whose columns are computed stays text and box-drawing only: a
    /// terminal substitutes whatever glyph its font carries for a pictograph,
    /// which is how one icon becomes a two-cell emoji and shears a padded row.
    /// The composer's key hints are exempt — `↑↓` there *names a key*, and it
    /// sits on a line nothing is aligned against.
    #[test]
    fn aligned_chrome_avoids_pictographs() {
        let snap = snap();
        let pal = pal();
        let mut rows = header_rows(&snap, &Conv::Channel("dev-room".into()), &pal, 60);
        rows.extend(empty_pane_rows(&pal, 60, 10));
        rows.extend(
            switcher_rows(
                &snap,
                &Switcher {
                    query: String::new(),
                    sel: 0,
                },
                &pal,
                60,
            )
            .0,
        );
        for line in texts(&rows) {
            assert!(
                line.chars().all(|c| !is_pictograph(c)),
                "ideographs get font-replaced as double-width: {line:?}"
            );
        }
    }

    /// The view paints no surface of its own any more: the terminal's background
    /// is the background. Only genuine marks may carry one, and never as a whole
    /// row — a row-level background is exactly the slab that was cut.
    #[test]
    fn nothing_paints_a_surface() {
        let snap = snap();
        let pal = pal();
        let posts = vec![Post {
            from: "scout".into(),
            you: false,
            at: 1_760_000_000,
            text: "found the regression".into(),
            kind: PostKind::Said,
        }];
        let ws = Workspace::default();
        let conv = Conv::Channel("dev-room".into());
        let mut all: Vec<Row> = Vec::new();
        all.extend(header_rows(&snap, &conv, &pal, 60));
        all.extend(message_rows(
            &posts,
            usize::MAX,
            &pal,
            60,
            &Avatars::default(),
        ));
        all.extend(composer_rows(&ws, &conv, &pal, 60).0);
        all.extend(empty_pane_rows(&pal, 60, 8));
        assert!(
            all.iter().all(|r| r.bg.is_none()),
            "no more full-row background"
        );
        assert!(blank_row().bg.is_none());

        // The overlay is the one exception, and it is an erase rather than a
        // colour: without it the avatar chip underneath glows through the box.
        let overlay = switcher_rows(
            &snap,
            &Switcher {
                query: String::new(),
                sel: 0,
            },
            &pal,
            60,
        )
        .0;
        assert!(
            overlay.iter().all(|r| r.bg == Some(Color::Reset)),
            "the overlay erases the whole row down to the terminal background"
        );
        // The one surviving background is the avatar chip, and only on its cells.
        let chip_cells: Vec<_> = message_rows(&posts, usize::MAX, &pal, 60, &Avatars::default())
            .iter()
            .flat_map(|r| r.line.segs.clone())
            .filter(|s| s.style.bg.is_some())
            .collect();
        assert_eq!(
            chip_cells.len(),
            1,
            "only the avatar cell carries a background: {chip_cells:?}"
        );
    }

    /// With images the portrait replaces the initial chip and spans two rows —
    /// the name row and the first body row — so the body still lines up under
    /// itself and the message costs no extra rows.
    #[test]
    fn image_avatars_occupy_the_gutter_without_costing_rows() {
        let pal = pal();
        let posts = vec![Post {
            from: "scout".into(),
            you: false,
            at: 1_760_000_000,
            text: "first line\nsecond line".into(),
            kind: PostKind::Said,
        }];
        let plain = message_rows(&posts, usize::MAX, &pal, 60, &Avatars::default());
        let imaged = message_rows(
            &posts,
            usize::MAX,
            &pal,
            60,
            &Avatars {
                images: true,
                pinned: Default::default(),
            },
        );
        assert_eq!(
            plain.len(),
            imaged.len(),
            "both skins produce the same row count"
        );

        let cells = |rows: &[Row]| -> Vec<String> {
            rows.iter()
                .map(|r| r.line.plain_text().chars().take(5).collect())
                .collect()
        };
        let head = imaged
            .iter()
            .position(|r| r.line.plain_text().contains("scout"))
            .unwrap_or_else(|| panic!("has a name row: {:?}", cells(&imaged)));
        let ph = crate::tui::gfx::PLACEHOLDER;
        assert!(
            imaged[head].line.plain_text().starts_with(ph),
            "the name row carries the top avatar half"
        );
        assert!(
            imaged[head + 1].line.plain_text().starts_with(ph),
            "the first body row carries the bottom half"
        );
        assert!(
            !imaged[head + 2].line.plain_text().starts_with(ph),
            "the second body row is plain indentation"
        );
        // Both halves address one image, and body text starts at the same column
        // whether the gutter holds the portrait or plain indentation.
        let id_of = |i: usize| imaged[i].line.segs.first().map(|s| s.style.fg);
        assert_eq!(id_of(head), id_of(head + 1), "same avatar");
        let body_col = |i: usize| -> usize {
            let segs = &imaged[i].line.segs;
            let body = segs.last().map(|s| s.text.clone()).unwrap_or_default();
            text_width(&imaged[i].line.plain_text()) - text_width(&body)
        };
        assert_eq!(
            body_col(head + 1),
            gutter(true),
            "first body row start column"
        );
        assert_eq!(
            body_col(head + 2),
            gutter(true),
            "second body row aligns to the same column"
        );
    }

    #[test]
    fn dm_rows_reuse_main_body_rendering_and_keep_the_existing_avatar_gutter() {
        let posts = vec![
            Post {
                from: "user".into(),
                you: true,
                at: 0,
                text: "plain **user** text".into(),
                kind: PostKind::Said,
            },
            Post {
                from: "scout".into(),
                you: false,
                at: 0,
                text: "## Result\n\nUse `cargo test`.".into(),
                kind: PostKind::Said,
            },
        ];
        let pal = pal();
        let theme = Theme::dark();
        let avatars = Avatars::default();
        let rows = dm_message_rows(&posts, usize::MAX, &pal, 60, &avatars, &theme);

        let user_head = rows
            .iter()
            .position(|row| row.line.plain_text().contains("You"))
            .unwrap_or_else(|| panic!("user name row missing"));
        let user_body = &rows[user_head + 1];
        assert_eq!(user_body.bg, None, "bubble must not repaint the DM gutter");
        assert!(
            user_body
                .line
                .segs
                .first()
                .is_some_and(|seg| seg.style.bg.is_none()),
            "existing avatar gutter keeps its own style"
        );
        assert!(
            user_body
                .line
                .segs
                .iter()
                .skip(1)
                .all(|seg| seg.style.bg == Some(theme.user_message_bg)),
            "main bubble background stays scoped to the body"
        );
        assert!(
            user_body
                .line
                .plain_text()
                .contains("❯ plain **user** text"),
            "main user bubble rows remain intact behind the gutter: {:?}",
            texts(&rows)
        );
        assert!(
            user_body.line.plain_text().starts_with("    "),
            "DM avatar gutter stays in front of the main body row"
        );

        let image_avatars = Avatars {
            images: true,
            ..Avatars::default()
        };
        let image_rows = dm_message_rows(&posts[..1], usize::MAX, &pal, 60, &image_avatars, &theme);
        let image_body = image_rows
            .iter()
            .find(|row| row.line.plain_text().contains("plain **user** text"))
            .unwrap_or_else(|| panic!("image-avatar user body missing"));
        assert!(
            image_body
                .line
                .segs
                .iter()
                .take(2)
                .all(|seg| seg.style.bg != Some(theme.user_message_bg)),
            "portrait and its spacer keep the existing DM gutter style"
        );
        assert!(
            image_body
                .line
                .segs
                .iter()
                .skip(2)
                .all(|seg| seg.style.bg == Some(theme.user_message_bg)),
            "the bubble begins only after the image-avatar gutter"
        );

        let scout_head = rows
            .iter()
            .position(|row| row.line.plain_text().contains("scout"))
            .unwrap_or_else(|| panic!("agent name row missing"));
        let assistant = &rows[scout_head + 1..];
        assert!(
            assistant
                .iter()
                .any(|row| row.line.plain_text().contains("⏺ Result")),
            "assistant uses the main markdown heading/prefix path: {:?}",
            texts(&rows)
        );
        assert!(
            assistant
                .iter()
                .any(|row| row.line.plain_text().contains("cargo test"))
        );
        assert!(assistant.iter().any(|row| {
            row.line
                .segs
                .iter()
                .any(|seg| seg.style.fg == Some(theme.code_fg))
        }));
    }

    #[test]
    fn queued_and_typing_posts_stay_visible() {
        let posts = vec![
            Post {
                from: "user".into(),
                you: true,
                at: 0,
                text: "check again".into(),
                kind: PostKind::Queued,
            },
            Post {
                from: "scout".into(),
                you: false,
                at: 0,
                text: "typing".into(),
                kind: PostKind::Typing,
            },
        ];
        let t = texts(&message_rows(
            &posts,
            usize::MAX,
            &pal(),
            60,
            &Avatars::default(),
        ));
        assert!(t.iter().any(|l| l.contains("pending delivery")), "{t:?}");
        assert!(t.iter().any(|l| l.contains("is typing")), "{t:?}");
    }

    /// A running turn shows prose from each round, hides every tool row, and
    /// keeps a trailing working state when the current block is a tool.
    #[test]
    fn a_running_turn_hides_tools_and_keeps_its_rounds_apart() {
        use crate::agents::LiveBlock;
        let live = vec![
            LiveBlock::Text("Let me verify the current state.".into()),
            LiveBlock::Tool("⏺ Bash($ git status)".into()),
            LiveBlock::Text("State confirmed. Checking the workflow.".into()),
            LiveBlock::Tool("⏺ Read(ci.yml)".into()),
            LiveBlock::Text("The quality job is the one failing.".into()),
        ];
        let posts = dm_posts(&[], &live, &[], "deploy", "user");
        let kinds: Vec<PostKind> = posts.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![PostKind::Said, PostKind::Said, PostKind::Typing],
            "only prose, with the tail typing: {posts:?}"
        );
        assert!(posts.iter().all(|p| matches!(
            p.kind,
            PostKind::Said | PostKind::Queued | PostKind::Typing | PostKind::Note
        )));
        assert!(
            posts
                .iter()
                .all(|p| !p.text.contains("state.Now") && !p.text.contains("workflow.The")),
            "{posts:?}"
        );

        assert_eq!(
            dm_posts(&[], &live[..2], &[], "deploy", "user")
                .iter()
                .map(|p| p.kind)
                .collect::<Vec<_>>(),
            vec![PostKind::Said, PostKind::Typing]
        );
        let only_tools = vec![LiveBlock::Tool("⏺ Bash($ git status)".into())];
        let tool_wait = dm_posts(&[], &only_tools, &[], "deploy", "user");
        assert_eq!(
            tool_wait.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![PostKind::Typing],
            "silent tool-only work still says it is working"
        );
        assert!(tool_wait.iter().all(|p| p.text.is_empty()));

        let between = vec![
            LiveBlock::Text("first round".into()),
            LiveBlock::Text(String::new()),
        ];
        assert_eq!(
            dm_posts(&[], &between, &[], "deploy", "user")
                .iter()
                .map(|p| p.kind)
                .collect::<Vec<_>>(),
            vec![PostKind::Said, PostKind::Typing]
        );
        assert!(dm_posts(&[], &[], &[], "deploy", "user").is_empty());
    }

    /// DM history and live turns show conversation text, never tool activity.
    /// A trailing working indicator remains while the hidden tool is running.
    #[test]
    fn dm_posts_hide_all_tool_activity_but_keep_working_feedback() {
        use crate::agents::LiveBlock;
        use crate::api::types::{ContentBlock, Message, Role};

        let history = vec![
            Message::user_text("check the repository"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "I will inspect it.".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "tool-1".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "git status"}),
                    },
                    ContentBlock::Text {
                        text: "The tree is clean.".into(),
                    },
                ],
            },
        ];
        let live = vec![
            LiveBlock::Text("Checking one more thing.".into()),
            LiveBlock::Tool("⏺ Read(src/main.rs)".into()),
        ];

        let posts = dm_posts(&history, &live, &[], "dev", "user");
        assert!(posts.iter().all(|post| matches!(
            post.kind,
            PostKind::Said | PostKind::Queued | PostKind::Typing | PostKind::Note
        )));
        assert!(
            posts.iter().all(|post| !post.text.contains("Bash")
                && !post.text.contains("Read(src/main.rs)")),
            "{posts:?}"
        );
        assert_eq!(
            posts
                .iter()
                .filter(|post| post.kind == PostKind::Typing)
                .count(),
            1,
            "hidden mid-tool work still has one visible working state: {posts:?}"
        );
    }

    #[test]
    fn composer_shows_the_placeholder_and_tracks_the_caret() {
        let ws = Workspace::default();
        let conv = Conv::Channel("dev-room".into());
        let (rows, caret) = composer_rows(&ws, &conv, &pal(), 50);
        let t = texts(&rows);
        assert!(t.iter().any(|l| l.contains("message #dev-room")), "{t:?}");
        assert!(t[0].contains('╭') && t.iter().any(|l| l.contains('╰')));
        assert_eq!(
            caret.map(|(_, col)| col),
            Some(3),
            "an empty draft's caret hugs the left border"
        );

        let typed = Workspace {
            composer: "change".into(),
            ..Workspace::default()
        };
        let (rows, caret) = composer_rows(&typed, &conv, &pal(), 50);
        assert!(texts(&rows).iter().any(|l| l.contains("change")));
        assert_eq!(
            caret.map(|(_, col)| col),
            Some(3 + 6),
            "the caret follows the text width"
        );

        // A DM addresses the instance, not a channel.
        let (rows, _) = composer_rows(&ws, &Conv::Dm("scout".into()), &pal(), 50);
        assert!(texts(&rows).iter().any(|l| l.contains("message scout")));
    }

    /// The title keeps its compact metadata row; small channels list each engine
    /// in one bounded row and larger rooms collapse to one engine summary.
    #[test]
    fn header_keeps_metadata_and_bounds_channel_engines() {
        let snap = snap();
        let t = texts(&header_rows(
            &snap,
            &Conv::Channel("dev-room".into()),
            &pal(),
            70,
        ));
        assert_eq!(t.len(), 3, "title + engines + divider: {t:?}");
        assert!(t[0].contains("# dev-room"), "{t:?}");
        assert!(t[0].contains("serial") && t[0].contains("4 msgs"), "{t:?}");
        assert!(t[0].contains("3 people"), "{t:?}");
        assert!(
            t[1].contains("main: main-model/high") && t[1].contains("scout: gpt-5.6-sol/max"),
            "small channel header names each agent engine: {t:?}"
        );
        assert!(
            t[0].trim_end().ends_with("bingo"),
            "workspace name right-aligned: {t:?}"
        );
        assert!(t[2].starts_with("─"), "{t:?}");

        let narrow = texts(&header_rows(
            &snap,
            &Conv::Channel("dev-room".into()),
            &pal(),
            24,
        ));
        assert_eq!(narrow.len(), 3, "header stays bounded: {narrow:?}");
        assert!(
            narrow[1].contains("2 agents · mixed"),
            "small rooms aggregate before names clip: {narrow:?}"
        );

        let mut large = snap.clone();
        for i in 0..3 {
            let name = format!("agent-{i}");
            large.dms.push(DmItem {
                name: name.clone(),
                state: AgentState::Running,
                model: format!("model-{i}"),
                thinking: Some("high".into()),
                description: "member".into(),
                unread: 0,
            });
            large.channels[0].members.push(name);
        }
        let large = texts(&header_rows(
            &large,
            &Conv::Channel("dev-room".into()),
            &pal(),
            32,
        ));
        assert_eq!(large.len(), 3, "header stays bounded: {large:?}");
        assert!(
            large[1].contains("5 agents · mixed engines"),
            "large rooms use a complete aggregate: {large:?}"
        );

        let t = texts(&header_rows(&snap, &Conv::Dm("qa".into()), &pal(), 70));
        assert!(t[0].contains("○ qa"), "{t:?}");
        assert!(
            t[0].contains("DM · claude-sonnet · high · idle · acceptance"),
            "DM metadata includes model, thinking, state, and description: {t:?}"
        );
    }

    #[test]
    fn switcher_filters_and_prefixes_by_kind() {
        let snap = snap();
        assert_eq!(switcher_matches(&snap, "").len(), 4);
        assert_eq!(
            switcher_matches(&snap, "de"),
            vec![
                Conv::Channel("dev-room".into()),
                Conv::Channel("design".into())
            ]
        );
        // A typed # or @ is a hint, not part of the needle.
        assert_eq!(
            switcher_matches(&snap, "@sco"),
            vec![Conv::Dm("scout".into())]
        );
        let sw = Switcher {
            query: "qa".into(),
            sel: 0,
        };
        let (rows, matches) = switcher_rows(&snap, &sw, &pal(), 40);
        assert_eq!(matches, vec![Conv::Dm("qa".into())]);
        assert!(texts(&rows).iter().any(|l| l.contains("@ qa")));
        // The overlay sits on top of the message list: every row must fill the full width, otherwise the text underneath shows through.
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(
                text_width(&r.line.plain_text()),
                40,
                "row {i} did not fill the width: {:?}",
                r.line.plain_text()
            );
        }
        // No match still draws a full box rather than collapsing to a border.
        let (rows, matches) = switcher_rows(
            &snap,
            &Switcher {
                query: "zzz".into(),
                sel: 0,
            },
            &pal(),
            40,
        );
        assert!(matches.is_empty());
        assert!(texts(&rows).iter().any(|l| l.contains("no matching")));
        assert!(rows.iter().all(|r| text_width(&r.line.plain_text()) == 40));
    }

    #[test]
    fn stepping_moves_the_selection_and_opens_what_it_lands_on() {
        let snap = snap();
        let mut ws = Workspace::default();
        ws.sync(&snap);
        assert_eq!(ws.open, Some(Conv::Channel("dev-room".into())));
        ws.step(&snap, 1);
        assert_eq!(ws.open, Some(Conv::Channel("design".into())));
        ws.step(&snap, 2);
        assert_eq!(ws.open, Some(Conv::Dm("qa".into())));
        // Clamped at both ends.
        ws.step(&snap, 5);
        assert_eq!(ws.sel, 3);
        ws.step(&snap, -9);
        assert_eq!(ws.sel, 0);

        // A conversation that disappears re-seats the selection instead of dangling.
        ws.select(Conv::Dm("gone".into()));
        ws.sync(&snap);
        assert_eq!(ws.open, Some(Conv::Channel("dev-room".into())));
    }

    #[test]
    fn read_cursors_are_per_conversation() {
        let mut ws = Workspace::default();
        let a = Conv::Channel("dev-room".into());
        ws.mark_read(&a, 3);
        ws.mark_read(&a, 1);
        assert_eq!(ws.read_cursor(&a), 3, "the cursor only advances");
        assert_eq!(ws.read_cursor(&Conv::Dm("scout".into())), 0);
    }
}
