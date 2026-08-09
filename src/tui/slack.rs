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
//! | app/bot messages | agent turns; tool calls read as attachments  |
//!
//! Everything here is a pure function of a [`Snapshot`] (what the session holds
//! right now) plus [`Workspace`] (where the eye is). The host loop in
//! [`crate::tui::entity`] owns the terminal, polls the snapshot each frame and
//! paints the three panes into their own `Rect`s — the row model itself stays
//! renderer-agnostic and testable, exactly like the rest of the display layer.

use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::agents::AgentState;
use crate::api::types::{ContentBlock, Message, Role};
use crate::channels::{ChannelMessage, ChannelMode};
use crate::tui::chat::Row;
use crate::tui::line::{Line, SegStyle, text_width, wrap_words};
use crate::tui::theme::Theme;

/// Rail width: a focus bar plus a two-glyph label.
const RAIL_W: u16 = 5;
/// Below this width the rail is dropped; the sidebar carries navigation alone.
const RAIL_MIN_TOTAL: u16 = 64;
/// Below this width the sidebar is dropped too and the conversation goes
/// full-bleed (Slack's own narrow-window behaviour).
const SIDEBAR_MIN_TOTAL: u16 = 44;
/// Left gutter of the message list: `avatar` + one space.
const GUTTER: usize = 4;
/// Consecutive messages from one sender inside this window are grouped under a
/// single name row (Slack groups on a 5-minute window).
const GROUP_WINDOW: u64 = 300;
/// Composer box grows to at most this many text rows before it scrolls.
const COMPOSER_MAX_ROWS: usize = 5;
/// Matches the quick switcher lists at once.
const SWITCHER_ROWS: usize = 8;

/// The workspace skin: Slack's *layout*, bingo's colours. Slack's aubergine was
/// tried first and cut — a saturated purple slab is a brand costume, and in a
/// terminal it reads as muddy next to everything else the app draws.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub rail_bg: Color,
    pub rail_active_bg: Color,
    pub side_bg: Color,
    pub side_text: Color,
    pub side_strong: Color,
    pub side_active_bg: Color,
    pub badge_bg: Color,
    pub badge_fg: Color,
    pub presence_on: Color,
    pub presence_off: Color,
    pub main_bg: Color,
    pub main_text: Color,
    pub main_dim: Color,
    pub divider: Color,
    pub accent: Color,
    pub unread: Color,
    pub send: Color,
    pub avatars: [Color; 6],
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    /// Accents come from the terminal theme, so the workspace moves with the
    /// rest of the app instead of pinning a second brand on top of it. Only the
    /// chrome greys are literal: they are warm neutrals chosen to sit under the
    /// theme's orange without turning muddy, and the terminal theme has no
    /// vocabulary for "sidebar".
    pub fn new(theme: &Theme) -> Self {
        let base = Palette {
            rail_bg: rgb(0x14110E),
            rail_active_bg: rgb(0x332A23),
            side_bg: rgb(0x1E1A16),
            side_text: rgb(0xA89C90),
            side_strong: rgb(0xF2ECE6),
            side_active_bg: theme.claude_deep,
            badge_bg: theme.claude_deep_strong,
            badge_fg: rgb(0xFFFFFF),
            presence_on: theme.success,
            presence_off: rgb(0x776C62),
            main_bg: rgb(0x1A1816),
            main_text: theme.text,
            main_dim: theme.inactive,
            divider: rgb(0x38332D),
            accent: theme.claude,
            unread: theme.claude_strong,
            send: theme.success,
            avatars: [
                rgb(0x4C9AE0),
                rgb(0x3FA96B),
                rgb(0xC9922E),
                rgb(0xCB5A74),
                rgb(0x7C6BD0),
                rgb(0xC1743C),
            ],
        };
        // The sidebar stays dark in a light terminal (Slack's own default does
        // the same); only the conversation pane turns over.
        let pal = if theme.is_dark {
            base
        } else {
            Palette {
                main_bg: rgb(0xFFFFFF),
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
            rail_bg: f(self.rail_bg),
            rail_active_bg: f(self.rail_active_bg),
            side_bg: f(self.side_bg),
            side_text: f(self.side_text),
            side_strong: f(self.side_strong),
            side_active_bg: f(self.side_active_bg),
            badge_bg: f(self.badge_bg),
            badge_fg: f(self.badge_fg),
            presence_on: f(self.presence_on),
            presence_off: f(self.presence_off),
            main_bg: f(self.main_bg),
            main_text: f(self.main_text),
            main_dim: f(self.main_dim),
            divider: f(self.divider),
            accent: f(self.accent),
            unread: f(self.unread),
            send: f(self.send),
            avatars: self.avatars.map(f),
        }
    }

    /// Stable per-sender avatar colour (Slack assigns one per member).
    fn avatar_of(&self, name: &str) -> Color {
        let hash = name
            .bytes()
            .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
        self.avatars[hash as usize % self.avatars.len()]
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

/// Rail tab: which slice of the workspace the sidebar lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Dms,
    Activity,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Home, Tab::Dms, Tab::Activity];

    fn label(self) -> &'static str {
        match self {
            Tab::Home => "主页",
            Tab::Dms => "私信",
            Tab::Activity => "动态",
        }
    }
}

/// One channel as the sidebar sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelItem {
    pub name: String,
    pub seq: u64,
    pub unread: u64,
    pub frozen: bool,
    pub mode: ChannelMode,
    pub members: Vec<String>,
}

/// One subagent instance as the sidebar sees it (a DM correspondent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmItem {
    pub name: String,
    pub state: AgentState,
    pub description: String,
    pub unread: u64,
}

/// Everything the view needs from the session, sampled once per frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub workspace: String,
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
    /// A tool call, rendered the way Slack renders an app attachment.
    Tool,
    /// Sent but still in the inbox — delivery happens at the next turn boundary.
    Queued,
    /// The streaming tail of a running turn (Slack's "…is typing").
    Typing,
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

/// Subagent history → posts. User turns are yours, assistant text is theirs, and
/// tool calls become attachments; tool results and thinking stay out (the main
/// transcript is where that detail lives).
pub fn dm_posts(
    history: &[Message],
    live: Option<&str>,
    pending: &[String],
    who: &str,
    me: &str,
) -> Vec<Post> {
    let mut out = Vec::new();
    for msg in history {
        for block in &msg.content {
            match (msg.role, block) {
                (Role::User, ContentBlock::Text { text }) => out.push(Post {
                    from: me.to_string(),
                    you: true,
                    at: 0,
                    text: text.clone(),
                    kind: PostKind::Said,
                }),
                (Role::Assistant, ContentBlock::Text { text }) => out.push(Post {
                    from: who.to_string(),
                    you: false,
                    at: 0,
                    text: text.clone(),
                    kind: PostKind::Said,
                }),
                (Role::Assistant, ContentBlock::ToolUse { name, input, .. }) => {
                    let glyph = crate::tui::activities::tool_glyph(name);
                    let shown = crate::tui::activities::display_tool_name(name);
                    let summary = crate::query::summarize_input(name, input);
                    let head = if summary.is_empty() {
                        format!("{glyph}{shown}")
                    } else {
                        format!("{glyph}{shown}({summary})")
                    };
                    out.push(Post {
                        from: who.to_string(),
                        you: false,
                        at: 0,
                        text: head,
                        kind: PostKind::Tool,
                    });
                }
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
    if let Some(live) = live
        && !live.trim().is_empty()
    {
        out.push(Post {
            from: who.to_string(),
            you: false,
            at: 0,
            text: live.to_string(),
            kind: PostKind::Typing,
        });
    }
    out
}

/// Which pane the keyboard is aimed at. Tab walks them left to right, skipping
/// whatever the terminal is too narrow to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Rail,
    Sidebar,
    Messages,
    Composer,
}

impl Focus {
    pub fn next(self, has_rail: bool, has_sidebar: bool) -> Focus {
        let order: Vec<Focus> = [
            Focus::Rail,
            Focus::Sidebar,
            Focus::Messages,
            Focus::Composer,
        ]
        .into_iter()
        .filter(|f| match f {
            Focus::Rail => has_rail,
            Focus::Sidebar => has_sidebar,
            _ => true,
        })
        .collect();
        let i = order
            .iter()
            .position(|f| *f == self)
            .map(|i| (i + 1) % order.len())
            .unwrap_or(order.len() - 1);
        order[i]
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
    pub tab: Tab,
    pub open: Option<Conv>,
    /// Selection index into `conversations(tab)`.
    pub sel: usize,
    pub focus: Focus,
    /// Rows scrolled up from the bottom (0 = pinned to the latest).
    pub scroll_up: usize,
    pub composer: String,
    pub switcher: Option<Switcher>,
    pub flash: Option<String>,
    /// Collapsed sections, in `[channels, dms]` order.
    pub collapsed: [bool; 2],
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
            tab: Tab::Home,
            open: None,
            sel: 0,
            focus: Focus::Composer,
            scroll_up: 0,
            composer: String::new(),
            switcher: None,
            flash: None,
            collapsed: [false, false],
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

    /// The sidebar's sections for the current tab: title, section index, items.
    /// The Activity tab lists what is unread *plus* whatever is open, so
    /// reading a conversation doesn't yank it out from under the cursor.
    pub fn sections(&self, snap: &Snapshot) -> Vec<(&'static str, usize, Vec<Conv>)> {
        let channels: Vec<Conv> = snap
            .channels
            .iter()
            .map(|c| Conv::Channel(c.name.clone()))
            .collect();
        let dms: Vec<Conv> = snap.dms.iter().map(|d| Conv::Dm(d.name.clone())).collect();
        match self.tab {
            Tab::Home => vec![("频道", 0, channels), ("私信", 1, dms)],
            Tab::Dms => vec![("私信", 1, dms)],
            Tab::Activity => vec![(
                "未读",
                0,
                snap.all()
                    .into_iter()
                    .filter(|c| snap.unread_of(c) > 0 || self.open.as_ref() == Some(c))
                    .collect(),
            )],
        }
    }

    /// Conversations you can actually land on: the tab's sections minus the
    /// collapsed ones.
    pub fn visible(&self, snap: &Snapshot) -> Vec<Conv> {
        self.sections(snap)
            .into_iter()
            .filter(|(_, idx, _)| !self.collapsed[*idx])
            .flat_map(|(_, _, items)| items)
            .collect()
    }

    /// Collapse or expand the section holding the current selection.
    pub fn fold_section(&mut self, snap: &Snapshot, collapsed: bool) {
        let Some(open) = self.open.clone() else {
            return;
        };
        if let Some((_, idx, _)) = self
            .sections(snap)
            .into_iter()
            .find(|(_, _, items)| items.contains(&open))
        {
            self.collapsed[idx] = collapsed;
        }
    }

    /// Move the sidebar selection and open what it lands on (Slack's alt+↑/↓).
    pub fn step(&mut self, snap: &Snapshot, delta: isize) {
        let convs = self.visible(snap);
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
            let fallback = self.visible(snap).first().cloned();
            self.open = fallback.or_else(|| all.first().cloned());
        }
        self.sel = self
            .open
            .as_ref()
            .and_then(|o| self.visible(snap).iter().position(|c| c == o))
            .unwrap_or(0);
    }
}

/// The three panes. `rail`/`sidebar` disappear as the terminal narrows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Panes {
    pub rail: Option<Rect>,
    pub sidebar: Option<Rect>,
    pub main: Rect,
}

pub fn layout(area: Rect) -> Panes {
    let rail_w = if area.width >= RAIL_MIN_TOTAL {
        RAIL_W
    } else {
        0
    };
    let side_w = if area.width >= SIDEBAR_MIN_TOTAL {
        (area.width / 4).clamp(18, 26)
    } else {
        0
    };
    let mut x = area.x;
    let rail = (rail_w > 0).then(|| {
        let r = Rect::new(x, area.y, rail_w, area.height);
        x += rail_w;
        r
    });
    let sidebar = (side_w > 0).then(|| {
        let r = Rect::new(x, area.y, side_w, area.height);
        x += side_w;
        r
    });
    let main = Rect::new(
        x,
        area.y,
        area.width.saturating_sub(x - area.x),
        area.height,
    );
    Panes {
        rail,
        sidebar,
        main,
    }
}

fn row(line: Line, bg: Color) -> Row {
    let mut r = Row::new(line);
    r.bg = Some(bg);
    r
}

fn blank(bg: Color) -> Row {
    row(Line::empty(), bg)
}

/// An empty conversation-pane row, for padding a short message list out to the
/// viewport so the pane's background reaches the composer.
pub fn blank_row(pal: &Palette) -> Row {
    blank(pal.main_bg)
}

/// Pad a pane out to its full height so its background covers the column
/// instead of stopping where the content does.
fn pad(mut rows: Vec<Row>, height: usize, bg: Color) -> Vec<Row> {
    rows.truncate(height);
    while rows.len() < height {
        rows.push(blank(bg));
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

/// Day-divider label: 今天 / 昨天 / M月D日. `None` when there is no clock to
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
        0 => "今天".to_string(),
        1 => "昨天".to_string(),
        _ => t.format("%-m月%-d日").to_string(),
    })
}

/// Rail: workspace chip on top, then one text label per tab.
pub fn rail_rows(snap: &Snapshot, ws: &Workspace, pal: &Palette, height: usize) -> Vec<Row> {
    let w = RAIL_W as usize;
    let initial = snap
        .workspace
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "·".to_string());
    let focused = ws.focus == Focus::Rail;
    let mut rows = vec![
        blank(pal.rail_bg),
        row(
            Line::styled(
                boxed(&format!(" {initial} "), w),
                SegStyle::fg(pal.side_strong).with_bg(pal.side_bg).bold(),
            ),
            pal.rail_bg,
        ),
        blank(pal.rail_bg),
    ];
    for tab in Tab::ALL {
        let active = tab == ws.tab;
        let (fg, bg) = if active {
            (pal.side_strong, pal.rail_active_bg)
        } else {
            (pal.side_text, pal.rail_bg)
        };
        // The label is the whole tab. Pictographic icons were tried and cut:
        // at one cell they are unreadable, and the terminal picks whichever
        // glyph the font happens to carry — often an emoji.
        let bar = if focused && active { "▎" } else { " " };
        let mut line = Line::styled(bar, SegStyle::fg(pal.side_strong).with_bg(pal.rail_bg));
        let style = if active {
            SegStyle::fg(pal.side_strong).with_bg(bg).bold()
        } else {
            SegStyle::fg(fg).with_bg(bg)
        };
        line.push_styled(boxed(tab.label(), w - 1), style);
        rows.push(row(line, pal.rail_bg));
        rows.push(blank(pal.rail_bg));
    }
    pad(rows, height, pal.rail_bg)
}

fn presence(state: AgentState, pal: &Palette) -> (&'static str, Color) {
    match state {
        AgentState::Running => ("●", pal.presence_on),
        AgentState::Idle => ("○", pal.presence_off),
        AgentState::Stopped => ("·", pal.presence_off),
    }
}

/// One sidebar conversation row: prefix glyph, name, unread badge. Active rows
/// take the blue bar, unread rows go bold white, read rows stay lavender.
fn conv_row(
    snap: &Snapshot,
    conv: &Conv,
    active: bool,
    focused: bool,
    pal: &Palette,
    width: usize,
) -> Row {
    let unread = snap.unread_of(conv);
    let bg = if active {
        pal.side_active_bg
    } else {
        pal.side_bg
    };
    let (glyph, glyph_fg) = match conv {
        Conv::Channel(_) => (
            "#".to_string(),
            if active || unread > 0 {
                pal.side_strong
            } else {
                pal.side_text
            },
        ),
        Conv::Dm(name) => {
            let state = snap
                .dm(name)
                .map(|d| d.state)
                .unwrap_or(AgentState::Stopped);
            let (g, c) = presence(state, pal);
            (g.to_string(), c)
        }
    };
    // Active and unread both read as "loud"; everything else is lavender.
    let loud = active || unread > 0;
    let name_style = if loud {
        SegStyle::fg(pal.side_strong).with_bg(bg).bold()
    } else {
        SegStyle::fg(pal.side_text).with_bg(bg)
    };
    // A frozen channel is struck through rather than given a glyph of its own:
    // it costs no columns and can't misalign on an ambiguous-width terminal.
    let name_style = match conv {
        Conv::Channel(name) if snap.channel(name).is_some_and(|c| c.frozen) => {
            name_style.strikethrough()
        }
        _ => name_style,
    };
    let badge = if unread > 0 {
        format!(" {unread} ")
    } else {
        String::new()
    };
    let cursor = if focused && active { "▎" } else { " " };
    let head = format!("{cursor} {glyph} ");
    let room = width
        .saturating_sub(text_width(&head))
        .saturating_sub(text_width(&badge));
    let name = crate::tui::chat::one_line(conv.name(), room.max(1));
    let mut line = Line::styled(cursor, SegStyle::fg(pal.side_strong).with_bg(bg));
    line.push_styled(" ", SegStyle::fg(glyph_fg).with_bg(bg));
    line.push_styled(glyph, SegStyle::fg(glyph_fg).with_bg(bg));
    line.push_styled(" ", SegStyle::fg(glyph_fg).with_bg(bg));
    line.push_styled(name.clone(), name_style);
    if !badge.is_empty() {
        let used = text_width(&head) + text_width(&name);
        let gap = width
            .saturating_sub(used)
            .saturating_sub(text_width(&badge));
        line.push_styled(" ".repeat(gap), SegStyle::fg(pal.side_text).with_bg(bg));
        line.push_styled(
            badge,
            SegStyle::fg(pal.badge_fg).with_bg(pal.badge_bg).bold(),
        );
    }
    row(line, bg)
}

fn section_row(title: &str, collapsed: bool, pal: &Palette) -> Row {
    let chevron = if collapsed { "▸" } else { "▾" };
    row(
        Line::styled(
            format!(" {chevron} {title}"),
            SegStyle::fg(pal.side_text).bold(),
        ),
        pal.side_bg,
    )
}

/// Sidebar: workspace header, quick-switcher hint, then the sections the
/// current tab shows.
pub fn sidebar_rows(
    snap: &Snapshot,
    ws: &Workspace,
    pal: &Palette,
    width: usize,
    height: usize,
) -> Vec<Row> {
    let focused = ws.focus == Focus::Sidebar;
    let mut rows = vec![
        row(
            Line::styled(
                format!(
                    " {}",
                    crate::tui::chat::one_line(&snap.workspace, width.saturating_sub(4))
                ),
                SegStyle::fg(pal.side_strong).bold(),
            ),
            pal.side_bg,
        ),
        row(
            Line::styled(" 跳转  ctrl+k", SegStyle::fg(pal.side_text)),
            pal.side_bg,
        ),
        blank(pal.side_bg),
    ];

    let mut body: Vec<Row> = Vec::new();
    for (title, idx, items) in ws.sections(snap) {
        if items.is_empty() {
            continue;
        }
        body.push(section_row(title, ws.collapsed[idx], pal));
        if ws.collapsed[idx] {
            continue;
        }
        for conv in items {
            let active = ws.open.as_ref() == Some(&conv);
            body.push(conv_row(snap, &conv, active, focused, pal, width));
        }
    }
    if body.is_empty() {
        body.push(row(
            Line::styled(
                match ws.tab {
                    Tab::Activity => " 没有未读",
                    _ => " 还没有频道或实例",
                },
                SegStyle::fg(pal.side_text).italic(),
            ),
            pal.side_bg,
        ));
    }

    // Keep the open conversation visible when the list outgrows the pane.
    let room = height.saturating_sub(rows.len());
    if body.len() > room {
        let anchor = body
            .iter()
            .position(|r| r.bg == Some(pal.side_active_bg))
            .unwrap_or(0);
        let start = anchor
            .saturating_sub(room.saturating_sub(2))
            .min(body.len().saturating_sub(room));
        body = body.into_iter().skip(start).collect();
    }
    rows.extend(body);
    pad(rows, height, pal.side_bg)
}

/// Conversation header: name, then a metadata line, then a rule.
pub fn header_rows(snap: &Snapshot, conv: &Conv, pal: &Palette, width: usize) -> Vec<Row> {
    let (title, meta, right) = match conv {
        Conv::Channel(name) => match snap.channel(name) {
            Some(c) => (
                format!(" # {name}"),
                format!(
                    "   {} · {} 条{}",
                    c.mode.label(),
                    c.seq,
                    if c.frozen { " · 已冻结" } else { "" }
                ),
                format!("{} 人 ", c.members.len()),
            ),
            None => (
                format!(" # {name}"),
                "   已不存在".to_string(),
                String::new(),
            ),
        },
        Conv::Dm(name) => match snap.dm(name) {
            Some(d) => {
                let (glyph, _) = presence(d.state, pal);
                (
                    format!(" {glyph} {name}"),
                    format!("   {} · {}", d.state.label(), d.description),
                    "私信 ".to_string(),
                )
            }
            None => (format!(" {name}"), "   已不存在".to_string(), String::new()),
        },
    };
    let mut head = Line::styled(
        crate::tui::chat::one_line(&title, width.saturating_sub(text_width(&right))),
        SegStyle::fg(pal.main_text).bold(),
    );
    let gap = width
        .saturating_sub(text_width(&head.plain_text()))
        .saturating_sub(text_width(&right));
    head.push_styled(" ".repeat(gap), SegStyle::fg(pal.main_dim));
    head.push_styled(right, SegStyle::fg(pal.main_dim));
    vec![
        row(head, pal.main_bg),
        row(
            Line::styled(
                crate::tui::chat::one_line(&meta, width),
                SegStyle::fg(pal.main_dim),
            ),
            pal.main_bg,
        ),
        row(
            Line::styled("─".repeat(width.min(500)), SegStyle::fg(pal.divider)),
            pal.main_bg,
        ),
    ]
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
    row(line, pal.main_bg)
}

/// Slack's unread marker: the rule itself goes red and the label sits at the
/// right end, so the eye catches the line before it reads the words.
fn unread_divider(pal: &Palette, width: usize) -> Row {
    let label = " 新消息 ";
    let w = text_width(label);
    let rule = width.saturating_sub(w).saturating_sub(1);
    let mut line = Line::styled("─".repeat(rule), SegStyle::fg(pal.unread));
    line.push_styled(label, SegStyle::fg(pal.unread).bold());
    row(line, pal.main_bg)
}

/// Avatar chip: the sender's initial on a per-sender colour.
fn avatar(name: &str, pal: &Palette) -> Line {
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
            .with_bg(pal.avatar_of(name))
            .bold(),
    )
}

/// The message list. Consecutive posts from one sender inside [`GROUP_WINDOW`]
/// share a name row; the day changes and the first unread message get dividers.
pub fn message_rows(posts: &[Post], unread_from: usize, pal: &Palette, width: usize) -> Vec<Row> {
    if posts.is_empty() {
        return vec![
            blank(pal.main_bg),
            row(
                Line::styled(
                    "   还没有消息。在下面写第一条。",
                    SegStyle::fg(pal.main_dim).italic(),
                ),
                pal.main_bg,
            ),
        ];
    }
    let body_w = width.saturating_sub(GUTTER + 1).max(8);
    let mut rows: Vec<Row> = Vec::new();
    let mut prev: Option<(&str, u64)> = None;
    let mut prev_day: Option<String> = None;
    for (i, post) in posts.iter().enumerate() {
        if let Some(label) = day_label(post.at)
            && prev_day.as_deref() != Some(label.as_str())
        {
            rows.push(blank(pal.main_bg));
            rows.push(day_divider(&label, pal, width));
            prev_day = Some(label);
            prev = None;
        }
        if i == unread_from && unread_from < posts.len() {
            rows.push(blank(pal.main_bg));
            rows.push(unread_divider(pal, width));
            prev = None;
        }
        let grouped = prev.is_some_and(|(from, at)| {
            from == post.from
                && post.kind != PostKind::Typing
                && (post.at == 0 || at == 0 || post.at.saturating_sub(at) <= GROUP_WINDOW)
        });
        if !grouped {
            rows.push(blank(pal.main_bg));
            let mut head = avatar(&post.from, pal);
            head.push_styled(" ", SegStyle::fg(pal.main_text));
            let shown = if post.you {
                "你".to_string()
            } else {
                post.from.clone()
            };
            head.push_styled(shown, SegStyle::fg(pal.main_text).bold());
            if !post.you {
                head.push_styled(" AGENT ", SegStyle::fg(pal.main_dim).with_bg(pal.divider));
            }
            let time = hhmm(post.at);
            if !time.is_empty() {
                head.push_styled(format!("  {time}"), SegStyle::fg(pal.main_dim));
            }
            rows.push(row(head, pal.main_bg));
        }
        let indent = " ".repeat(GUTTER);
        match post.kind {
            PostKind::Tool => {
                let text = crate::tui::chat::one_line(&post.text, body_w);
                let mut line = Line::styled(indent.clone(), SegStyle::fg(pal.main_dim));
                line.push_styled("▏", SegStyle::fg(pal.accent));
                line.push_styled(format!(" {text}"), SegStyle::fg(pal.main_dim));
                rows.push(row(line, pal.main_bg));
            }
            _ => {
                let style = match post.kind {
                    PostKind::Queued => SegStyle::fg(pal.main_dim),
                    _ => SegStyle::fg(pal.main_text),
                };
                for para in post.text.lines() {
                    let wrapped = wrap_words(para, body_w);
                    if wrapped.is_empty() {
                        rows.push(blank(pal.main_bg));
                    }
                    for l in wrapped {
                        rows.push(row(
                            Line::styled(format!("{indent}{l}"), style),
                            pal.main_bg,
                        ));
                    }
                }
                if post.kind == PostKind::Queued {
                    rows.push(row(
                        Line::styled(
                            format!("{indent}⧖ 待送达（下一个回合边界注入）"),
                            SegStyle::fg(pal.main_dim).italic(),
                        ),
                        pal.main_bg,
                    ));
                }
                if post.kind == PostKind::Typing {
                    rows.push(row(
                        Line::styled(
                            format!("{indent}✻ {} 正在输入…", post.from),
                            SegStyle::fg(pal.accent).italic(),
                        ),
                        pal.main_bg,
                    ));
                }
            }
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
        Conv::Channel(name) => format!("给 #{name} 发消息"),
        Conv::Dm(name) => format!("给 {name} 发消息"),
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
        row(
            Line::styled(
                format!(" {left}{}{right}", "─".repeat(inner + 2)),
                SegStyle::fg(border),
            ),
            pal.main_bg,
        )
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
        rows.push(row(line, pal.main_bg));
    }
    rows.push(frame("╰", "╯"));
    let hint = if active {
        "  enter 发送 · tab 切换焦点 · ctrl+k 跳转 · esc 返回"
    } else {
        "  tab 回到输入框 · ↑↓ 滚动 · alt+↑↓ 换会话 · esc 返回"
    };
    let foot = Line::styled(
        crate::tui::chat::one_line(hint, width.saturating_sub(2)),
        SegStyle::fg(pal.main_dim),
    );
    rows.push(row(foot, pal.main_bg));
    if let Some(flash) = &ws.flash {
        rows.push(row(
            Line::styled(
                crate::tui::chat::one_line(&format!("  ⚠ {flash}"), width),
                SegStyle::fg(pal.unread),
            ),
            pal.main_bg,
        ));
    }
    (rows, caret)
}

/// What the conversation pane shows when there is nothing to open yet.
pub fn empty_pane_rows(pal: &Palette, width: usize, height: usize) -> Vec<Row> {
    let lines = [
        ("这里还没有会话", true),
        ("", false),
        ("Agent 工具派生实例后会出现在「私信」，", false),
        ("Channel 工具建的房间会出现在「频道」。", false),
        ("", false),
        ("esc 返回", false),
    ];
    let top = height.saturating_sub(lines.len()) / 2;
    let mut rows = vec![blank(pal.main_bg); top];
    for (text, strong) in lines {
        let style = if strong {
            SegStyle::fg(pal.main_text).bold()
        } else {
            SegStyle::fg(pal.main_dim)
        };
        rows.push(row(Line::styled(boxed(text, width), style), pal.main_bg));
    }
    pad(rows, height, pal.main_bg)
}

/// Quick-switcher overlay rows and the matches they list (ctrl+K).
pub fn switcher_rows(
    snap: &Snapshot,
    sw: &Switcher,
    pal: &Palette,
    width: usize,
) -> (Vec<Row>, Vec<Conv>) {
    let matches = switcher_matches(snap, &sw.query);
    // Same box geometry as the composer: margin + │ + space + inner + space + │.
    let inner = width.saturating_sub(5).max(8);
    // The overlay sits on top of the message list, so every row has to be
    // opaque across the full pane — repainting only the background would leave
    // the glyphs underneath showing through.
    let boxed_row = |content: Line, used: usize| {
        let mut line = Line::styled(" │ ", SegStyle::fg(pal.accent));
        line.segs.extend(content.segs);
        line.push_styled(
            " ".repeat(inner.saturating_sub(used) + 1),
            SegStyle::fg(pal.main_text),
        );
        line.push_styled("│", SegStyle::fg(pal.accent));
        row(line, pal.main_bg)
    };
    let rule = |left: &str, right: &str| {
        row(
            Line::styled(
                format!(" {left}{}{right}", "─".repeat(inner + 2)),
                SegStyle::fg(pal.accent),
            ),
            pal.main_bg,
        )
    };

    let mut rows = vec![rule("╭", "╮")];
    let query_text = crate::tui::chat::one_line(&sw.query, inner.saturating_sub(5));
    let mut query = Line::styled("跳转 ", SegStyle::fg(pal.main_dim));
    query.push_styled(query_text.clone(), SegStyle::fg(pal.main_text));
    rows.push(boxed_row(query, 5 + text_width(&query_text)));

    if matches.is_empty() {
        let text = "没有匹配的会话";
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
        let label = crate::tui::chat::one_line(&label, inner);
        let style = if sel {
            SegStyle::fg(pal.badge_fg)
                .with_bg(pal.side_active_bg)
                .bold()
        } else {
            SegStyle::fg(pal.main_text)
        };
        // The selection bar runs the width of the row, Slack-style, so pad
        // inside the highlighted segment rather than after it.
        let used = text_width(&label);
        let line = if sel {
            Line::styled(format!("{label}{}", " ".repeat(inner - used)), style)
        } else {
            Line::styled(label, style)
        };
        rows.push(boxed_row(line, inner.min(if sel { inner } else { used })));
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
                    description: "侦察".into(),
                    unread: 0,
                },
                DmItem {
                    name: "qa".into(),
                    state: AgentState::Idle,
                    description: "验收".into(),
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
    fn layout_sheds_panes_as_the_terminal_narrows() {
        let wide = layout(Rect::new(0, 0, 120, 30));
        assert_eq!(wide.rail.map(|r| r.width), Some(RAIL_W));
        assert_eq!(wide.sidebar.map(|r| r.width), Some(26));
        assert_eq!(wide.main.x, RAIL_W + 26);
        assert_eq!(wide.main.width, 120 - RAIL_W - 26);

        // Rail drops first, sidebar survives.
        let mid = layout(Rect::new(0, 0, 50, 30));
        assert!(mid.rail.is_none());
        assert_eq!(mid.sidebar.map(|r| r.width), Some(18));
        assert_eq!(mid.main.width, 50 - 18);

        // Narrow: the conversation goes full-bleed.
        let narrow = layout(Rect::new(0, 0, 40, 30));
        assert!(narrow.rail.is_none() && narrow.sidebar.is_none());
        assert_eq!(narrow.main.width, 40);
    }

    #[test]
    fn sidebar_lists_sections_with_unread_and_active_styling() {
        let snap = snap();
        let mut ws = Workspace {
            open: Some(Conv::Channel("dev-room".into())),
            focus: Focus::Sidebar,
            ..Workspace::default()
        };
        ws.sync(&snap);
        let rows = sidebar_rows(&snap, &ws, &pal(), 24, 20);
        let t = texts(&rows);
        assert!(t.iter().any(|l| l.starts_with(" bingo")), "{t:?}");
        assert!(t.iter().any(|l| l.contains("▾ 频道")));
        assert!(t.iter().any(|l| l.contains("▾ 私信")));
        // Channels carry #, DMs a presence dot keyed to the instance state.
        assert!(t.iter().any(|l| l.contains("# dev-room")));
        assert!(t.iter().any(|l| l.contains("# design")));
        assert!(t.iter().any(|l| l.contains("● scout")));
        assert!(t.iter().any(|l| l.contains("○ qa")));
        // A frozen channel is struck through, costing no columns.
        let frozen = rows
            .iter()
            .find(|r| r.line.plain_text().contains("design"))
            .unwrap_or_else(|| panic!("有 design 行"));
        assert!(frozen.line.segs.iter().any(|s| s.style.strikethrough));
        // Unread badges ride the right edge.
        assert!(t.iter().any(|l| l.contains("dev-room") && l.contains("2")));
        assert!(t.iter().any(|l| l.contains("qa") && l.contains("3")));
        // The open conversation takes the blue bar.
        let active = rows
            .iter()
            .find(|r| r.line.plain_text().contains("dev-room"))
            .unwrap_or_else(|| panic!("有 dev-room 行"));
        assert_eq!(active.bg, Some(pal().side_active_bg));
        // Every row paints the column so the aubergine reaches the bottom.
        assert_eq!(rows.len(), 20);
        assert!(rows.iter().all(|r| r.bg.is_some()));
    }

    /// Every pane paints its own column edge to edge. Without this the
    /// aubergine would hug the glyphs and the terminal's own background would
    /// show through the gaps — the one thing that would give the skin away.
    #[test]
    fn panes_paint_their_full_column() {
        use ratatui::buffer::Buffer;
        let snap = snap();
        let mut ws = Workspace {
            open: Some(Conv::Channel("dev-room".into())),
            ..Workspace::default()
        };
        ws.sync(&snap);
        let pal = pal();
        let area = Rect::new(0, 0, 80, 12);
        let panes = layout(area);
        let mut buf = Buffer::empty(area);
        let side = panes.sidebar.unwrap_or_else(|| panic!("有侧栏"));
        crate::tui::view::render_rows(
            &sidebar_rows(&snap, &ws, &pal, side.width as usize, 12),
            pal.main_text,
            &mut buf,
            side,
        );
        crate::tui::view::render_rows(
            &rail_rows(&snap, &ws, &pal, 12),
            pal.main_text,
            &mut buf,
            panes.rail.unwrap_or_else(|| panic!("有 rail")),
        );
        // The active row is blue from its first cell to its last, badge included.
        let y = (0..12)
            .find(|y| {
                (side.x..side.right())
                    .any(|x| buf[(x, *y)].bg == pal.side_active_bg && buf[(x, *y)].symbol() == "d")
            })
            .unwrap_or_else(|| panic!("找得到 dev-room 行"));
        for x in side.x..side.right() {
            let bg = buf[(x, y)].bg;
            assert!(
                bg == pal.side_active_bg || bg == pal.badge_bg,
                "x={x} 上的背景断了: {bg:?}"
            );
        }
        // Rail and sidebar own every cell of their columns, on every row.
        for y in 0..12 {
            for x in panes.rail.iter().flat_map(|r| r.x..r.right()) {
                assert_ne!(
                    buf[(x, y)].bg,
                    ratatui::style::Color::Reset,
                    "rail ({x},{y})"
                );
            }
            for x in side.x..side.right() {
                assert_ne!(
                    buf[(x, y)].bg,
                    ratatui::style::Color::Reset,
                    "侧栏 ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn activity_tab_lists_unread_plus_whatever_is_open() {
        let snap = snap();
        let ws = Workspace {
            tab: Tab::Activity,
            ..Workspace::default()
        };
        assert_eq!(
            ws.visible(&snap),
            vec![Conv::Channel("dev-room".into()), Conv::Dm("qa".into())]
        );
        // Reading a conversation must not yank it out from under the cursor.
        let reading = Workspace {
            open: Some(Conv::Channel("design".into())),
            ..ws.clone()
        };
        assert!(
            reading
                .visible(&snap)
                .contains(&Conv::Channel("design".into()))
        );

        let dms = Workspace {
            tab: Tab::Dms,
            ..Workspace::default()
        };
        assert_eq!(dms.visible(&snap).len(), 2);
        assert_eq!(Workspace::default().visible(&snap).len(), 4);
    }

    #[test]
    fn collapsing_a_section_takes_its_rows_out_of_navigation() {
        let snap = snap();
        let mut ws = Workspace {
            open: Some(Conv::Channel("dev-room".into())),
            ..Workspace::default()
        };
        ws.fold_section(&snap, true);
        assert_eq!(ws.collapsed, [true, false]);
        assert_eq!(
            ws.visible(&snap),
            vec![Conv::Dm("scout".into()), Conv::Dm("qa".into())],
            "折叠的频道段不再参与 ↑↓"
        );
        // The header row survives with a ▸ chevron; its members are gone.
        let t = texts(&sidebar_rows(&snap, &ws, &pal(), 24, 20));
        assert!(t.iter().any(|l| l.contains("▸ 频道")), "{t:?}");
        assert!(!t.iter().any(|l| l.contains("dev-room")), "{t:?}");
        ws.fold_section(&snap, false);
        assert_eq!(ws.visible(&snap).len(), 4);
    }

    #[test]
    fn messages_group_by_sender_and_break_on_unread() {
        let base = 1_760_000_000u64;
        let posts = vec![
            Post {
                from: "scout".into(),
                you: false,
                at: base,
                text: "找到回归了".into(),
                kind: PostKind::Said,
            },
            Post {
                from: "scout".into(),
                you: false,
                at: base + 30,
                text: "在 term.rs".into(),
                kind: PostKind::Said,
            },
            Post {
                from: "user".into(),
                you: true,
                at: base + 900,
                text: "收到".into(),
                kind: PostKind::Said,
            },
        ];
        let rows = message_rows(&posts, 2, &pal(), 60);
        let t = texts(&rows);
        // The second scout message is grouped: one name row, not two.
        assert_eq!(
            t.iter()
                .filter(|l| l.contains("scout") && l.contains("AGENT"))
                .count(),
            1,
            "{t:?}"
        );
        // Your own messages are labelled 你 and carry no AGENT badge.
        let mine = t
            .iter()
            .find(|l| l.contains("你"))
            .unwrap_or_else(|| panic!("有你的名字行"));
        assert!(!mine.contains("AGENT"), "{mine:?}");
        // Unread divider sits before the third post.
        assert!(t.iter().any(|l| l.contains("新消息")), "{t:?}");
        // Bodies sit in the avatar gutter.
        assert!(t.iter().any(|l| l.starts_with("    找到回归了")), "{t:?}");
        // Empty log gets the invitation, not a blank pane.
        assert!(
            texts(&message_rows(&[], 0, &pal(), 60))
                .iter()
                .any(|l| l.contains("还没有消息"))
        );
    }

    #[test]
    fn tool_calls_render_as_attachments_and_queued_posts_stay_visible() {
        let posts = vec![
            Post {
                from: "scout".into(),
                you: false,
                at: 0,
                text: "⏺ Bash($ rg lazy)".into(),
                kind: PostKind::Tool,
            },
            Post {
                from: "user".into(),
                you: true,
                at: 0,
                text: "再查一遍".into(),
                kind: PostKind::Queued,
            },
            Post {
                from: "scout".into(),
                you: false,
                at: 0,
                text: "正在写".into(),
                kind: PostKind::Typing,
            },
        ];
        let t = texts(&message_rows(&posts, usize::MAX, &pal(), 60));
        assert!(t.iter().any(|l| l.contains("▏ ⏺ Bash")), "{t:?}");
        assert!(t.iter().any(|l| l.contains("待送达")), "{t:?}");
        assert!(t.iter().any(|l| l.contains("正在输入")), "{t:?}");
    }

    #[test]
    fn composer_shows_the_placeholder_and_tracks_the_caret() {
        let ws = Workspace::default();
        let conv = Conv::Channel("dev-room".into());
        let (rows, caret) = composer_rows(&ws, &conv, &pal(), 50);
        let t = texts(&rows);
        assert!(t.iter().any(|l| l.contains("给 #dev-room 发消息")), "{t:?}");
        assert!(t[0].contains('╭') && t.iter().any(|l| l.contains('╰')));
        assert_eq!(caret.map(|(_, col)| col), Some(3), "空稿的光标贴左边框");

        let typed = Workspace {
            composer: "先别改".into(),
            ..Workspace::default()
        };
        let (rows, caret) = composer_rows(&typed, &conv, &pal(), 50);
        assert!(texts(&rows).iter().any(|l| l.contains("先别改")));
        assert_eq!(caret.map(|(_, col)| col), Some(3 + 6), "光标跟着字宽走");

        // A DM addresses the instance, not a channel.
        let (rows, _) = composer_rows(&ws, &Conv::Dm("scout".into()), &pal(), 50);
        assert!(texts(&rows).iter().any(|l| l.contains("给 scout 发消息")));
    }

    #[test]
    fn header_reports_mode_members_and_presence() {
        let snap = snap();
        let t = texts(&header_rows(
            &snap,
            &Conv::Channel("dev-room".into()),
            &pal(),
            60,
        ));
        assert!(t[0].contains("# dev-room"), "{t:?}");
        assert!(t[0].contains("3 人"), "{t:?}");
        assert!(t[1].contains("serial") && t[1].contains("4 条"));
        let t = texts(&header_rows(&snap, &Conv::Dm("qa".into()), &pal(), 60));
        assert!(t[0].contains("○ qa"), "{t:?}");
        assert!(t[1].contains("idle") && t[1].contains("验收"));
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
        // The overlay覆盖在消息列表之上：每行都要写满整宽，否则底下的字会透上来。
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(
                text_width(&r.line.plain_text()),
                40,
                "第 {i} 行没写满: {:?}",
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
        assert!(texts(&rows).iter().any(|l| l.contains("没有匹配")));
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
        assert_eq!(ws.read_cursor(&a), 3, "游标只前进");
        assert_eq!(ws.read_cursor(&Conv::Dm("scout".into())), 0);
    }

    #[test]
    fn rail_marks_the_active_tab() {
        let snap = snap();
        let ws = Workspace {
            tab: Tab::Dms,
            ..Workspace::default()
        };
        let rows = rail_rows(&snap, &ws, &pal(), 20);
        let t = texts(&rows);
        assert!(t.iter().any(|l| l.contains('B')), "工作区首字母: {t:?}");
        // Labels only — no pictographic icons to get substituted by an emoji.
        assert!(
            t.iter().all(|l| l.chars().all(|c| !is_pictograph(c))),
            "{t:?}"
        );
        let active = rows
            .iter()
            .find(|r| r.line.plain_text().contains("私信"))
            .unwrap_or_else(|| panic!("有私信行"));
        assert!(
            active
                .line
                .segs
                .iter()
                .any(|s| s.style.bg == Some(pal().rail_active_bg))
        );
        assert_eq!(rows.len(), 20);
    }
}
