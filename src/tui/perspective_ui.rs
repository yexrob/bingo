//! The perspective page's modal (D96): two levels, read-only, over one snapshot.
//!
//! Shape follows the transcript view (D82): a self-driving alt-screen loop that
//! owns the terminal while it is open, with an `already_alt` flag so the
//! fullscreen host does not nest a second alternate screen inside its own. It
//! therefore takes **no `EscLayer` slot** — a modal that owns every key while it
//! is up has nothing to hand the layer walker, which is the same call
//! `run_transcript_modal` makes.
//!
//! **Two levels in one loop.** The index and a thread are not two surfaces that
//! happen to be reachable from one another; they are one page at two depths,
//! sharing a snapshot, a theme and a frame. Splitting them into two modals would
//! have meant two loops, two claims on the terminal and a snapshot rebuilt on
//! every Enter — which is precisely the live-ness the page does not want.
//!
//! **The pager is [`TranscriptState`]**, unchanged: scrolling, paging, `g`/`G`
//! and `/` search with `n`/`N` are the transcript's, so a reader who has used
//! `ctrl+o` already knows this view. `ctrl+e` is deliberately **not** bound —
//! there it toggles fold state that lives on the hub's messages, and a thread's
//! rows are [`crate::tui::buffer::Post`]s, which carry a tool call as one
//! collapsed line and no output at all. There is no second state to show.

use std::io::stdout;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::tui::buffer::stamp;
use crate::tui::bufferview::settled_post_rows;
use crate::tui::chat::{Chat, Row, one_line};
use crate::tui::line::{Line, Seg, SegStyle, text_width};
use crate::tui::perspective::{Dossier, LaneId, dossier};
use crate::tui::theme::Theme;
use crate::tui::transcript::{self, TranscriptState};
use crate::tui::view;

/// Which depth of the page is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The grouped list of threads.
    Index,
    /// One thread, paged.
    Thread,
}

/// What the pure key handler asks the loop to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Open the selected lane (the loop builds its rows at the current width).
    Open,
    /// Thread → index.
    Back,
    /// Close the whole page.
    Close,
}

/// The page's state: a snapshot, a cursor, and — once a thread is open — the
/// transcript's pager over that thread's rows.
pub struct PerspectiveState {
    pub page: Dossier,
    pub level: Level,
    /// Cursor into [`Dossier::lanes`], not into rows: the group headings are
    /// furniture and the cursor must not be able to land on them (the D95
    /// directory's rule).
    pub selected: usize,
    pub pager: Option<TranscriptState>,
}

impl PerspectiveState {
    pub fn new(page: Dossier) -> Self {
        Self {
            page,
            level: Level::Index,
            selected: 0,
            pager: None,
        }
    }

    pub fn selected_lane(&self) -> Option<&crate::tui::perspective::Lane> {
        self.page.lanes.get(self.selected)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.page.lanes.is_empty() {
            return;
        }
        let last = self.page.lanes.len() - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(d.unsigned_abs()),
            d => (self.selected + d as usize).min(last),
        };
    }

    /// The title the current level opens under.
    pub fn title(&self) -> String {
        match (self.level, self.selected_lane()) {
            (Level::Thread, Some(lane)) => lane.id.title(&self.page.agent),
            _ => format!("@{} · perspective · read-only", self.page.agent),
        }
    }
}

/// One key. Pure: the loop owns the terminal, this owns the meaning.
pub fn on_key(state: &mut PerspectiveState, code: KeyCode, modifiers: KeyModifiers) -> Action {
    match state.level {
        Level::Index => match code {
            KeyCode::Char('q') | KeyCode::Esc => Action::Close,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Action::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                state.move_selection(-1);
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.move_selection(1);
                Action::None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                state.selected = 0;
                Action::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                state.selected = state.page.lanes.len().saturating_sub(1);
                Action::None
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Tab => {
                if state.selected_lane().is_some() {
                    Action::Open
                } else {
                    Action::None
                }
            }
            _ => Action::None,
        },
        Level::Thread => {
            let Some(pager) = &mut state.pager else {
                return Action::Back;
            };
            // While the search input is open it owns every key, including Esc
            // (which cancels the search) and `q` (which is a letter). Only once
            // it is closed do the level's own keys mean anything.
            if pager.input.is_none() {
                match code {
                    KeyCode::Char('q') => return Action::Close,
                    KeyCode::Esc | KeyCode::Left => return Action::Back,
                    _ => {}
                }
            }
            match transcript::on_key(pager, code, modifiers) {
                // The transcript's own closers (`ctrl+o`, `ctrl+c`) close the
                // page; `q`/`Esc` never reach here.
                transcript::Action::Close => Action::Close,
                // Nothing in a thread rebuilds: the rows are a snapshot and
                // `ctrl+e` is unbound.
                // A thread's rows come from a read-only snapshot and carry no
                // image blocks of their own, so `o` has nothing to open here.
                transcript::Action::Rebuild
                | transcript::Action::OpenImage(_)
                | transcript::Action::None => Action::None,
            }
        }
    }
}

/// Which heading a lane sits under. Timeline is its own row at the top and
/// needs no heading over one item.
fn group_of(id: &LaneId) -> Option<&'static str> {
    match id {
        LaneId::Timeline => None,
        LaneId::Dm(_) => Some("direct messages"),
        LaneId::Room(_) => Some("rooms"),
        LaneId::Intake => Some("intake"),
    }
}

/// `6 messages` / `1 message` — the count the index prints beside a lane.
fn count_label(n: usize) -> String {
    if n == 1 {
        "1 message".to_string()
    } else {
        format!("{n} messages")
    }
}

/// The index: a rule, then the lanes under their group headings.
pub fn index_rows(state: &PerspectiveState, width: usize, theme: &Theme) -> Vec<Row> {
    let mut rows = vec![
        Row::new(Line::styled(
            one_line(&format!("── {} ──", state.title()), width),
            SegStyle::fg(theme.text_muted),
        )),
        Row::new(Line::empty()),
    ];
    if state.page.is_empty() {
        rows.push(Row::new(Line::styled(
            one_line(
                &format!("@{} has not talked to anybody yet", state.page.agent),
                width,
            ),
            SegStyle::fg(theme.text_secondary),
        )));
        return rows;
    }
    let mut group: Option<&'static str> = None;
    for (i, lane) in state.page.lanes.iter().enumerate() {
        let heading = group_of(&lane.id);
        if heading != group {
            group = heading;
            if let Some(heading) = heading {
                rows.push(Row::new(Line::empty()));
                rows.push(Row::new(Line::styled(
                    one_line(heading, width),
                    SegStyle::fg(theme.text_muted),
                )));
            }
        }
        rows.push(lane_row(state, i, width, theme));
    }
    rows
}

/// One lane: cursor, name, then the count and the clock in the muted tier —
/// the name is what you are looking for, the rest is what tells you whether to
/// open it.
fn lane_row(state: &PerspectiveState, index: usize, width: usize, theme: &Theme) -> Row {
    let lane = &state.page.lanes[index];
    let selected = index == state.selected;
    let name_style = if selected {
        SegStyle::fg(theme.permission)
    } else {
        SegStyle::fg(theme.text)
    };
    let cursor = if selected { "❯ " } else { "  " };
    let name = lane.id.label();
    let mut trailing = count_label(lane.messages());
    let at = stamp(lane.last_at);
    if !at.is_empty() {
        trailing.push_str(&format!(" · {at}"));
    }
    // The trailing block is flush right where the row is wide enough to hold
    // both with a gap, and dropped rather than wrapped where it is not (D93:
    // content wins).
    let left = format!("{cursor}{name}");
    let left_w = text_width(&left);
    let right_w = text_width(&trailing);
    let mut segs = vec![Seg {
        text: one_line(&left, width),
        style: name_style,
    }];
    if width > left_w + right_w + 2 {
        segs.push(Seg {
            text: " ".repeat(width - left_w - right_w),
            style: SegStyle::plain(),
        });
        segs.push(Seg {
            text: trailing,
            style: SegStyle::fg(theme.text_muted),
        });
    }
    Row::new(Line { segs, image: None })
}

/// A thread: its rule, then its posts through the conversation engine's own
/// renderer. The protagonist's process rows are part of the thread, which is
/// the whole point of the page.
pub fn thread_rows(
    state: &PerspectiveState,
    width: usize,
    theme: &Theme,
    gutter: Option<&crate::tui::avatar::Gutter<'_>>,
) -> Vec<Row> {
    let Some(lane) = state.selected_lane() else {
        return Vec::new();
    };
    let mut rows = vec![
        Row::new(Line::styled(
            one_line(
                &format!("── {} ──", lane.id.title(&state.page.agent)),
                width,
            ),
            SegStyle::fg(theme.text_muted),
        )),
        Row::new(Line::empty()),
    ];
    // The thread is a conversation, so it wears the conversation's gutter —
    // the same runs, the same portraits, the same width arithmetic the DM and
    // room views use, because it is the same builder underneath.
    let runs = crate::tui::bufferview::sender_runs(&lane.posts);
    for (post, lead) in lane.posts.iter().zip(runs) {
        let sender = gutter.map(|g| crate::tui::bufferview::Sender {
            gutter: *g,
            index: g.index_for(&post.from),
            lead,
        });
        rows.extend(settled_post_rows(theme, post, width, sender.as_ref()));
    }
    rows
}

/// The hint bar. Two tiers, longest first — the widest that fits whole wins,
/// exactly as the transcript's footer chooses.
fn hint_tiers(level: Level) -> [&'static str; 2] {
    match level {
        Level::Index => ["↑/↓ select · enter open · q close", "↑/↓ · enter · q"],
        Level::Thread => [
            "j/k scroll · g/G ends · / search · n/N hits · esc back · q close",
            "j/k · / search · esc back · q",
        ],
    }
}

/// The footer: where you are, how far down, and what the keys do.
pub fn footer(state: &PerspectiveState, width: usize, theme: &Theme) -> Line {
    if let Some(pager) = &state.pager
        && let Some(input) = &pager.input
    {
        return Line::styled(
            one_line(&format!("/{input}▌  enter search · esc cancel"), width),
            SegStyle::fg(theme.text),
        );
    }
    let mut left = match state.level {
        Level::Index => "perspective".to_string(),
        Level::Thread => state
            .selected_lane()
            .map(|lane| lane.id.label())
            .unwrap_or_else(|| "perspective".to_string()),
    };
    if let Some(pager) = &state.pager
        && state.level == Level::Thread
    {
        left.push_str(&format!("  {}%", pager.percent()));
        if !pager.query.is_empty() {
            left.push_str(&if pager.matches.is_empty() {
                " · no match".to_string()
            } else {
                format!(" · {}/{}", pager.current + 1, pager.matches.len())
            });
        }
    }
    let mut segs = vec![Seg {
        text: left.clone(),
        style: SegStyle::fg(theme.text_secondary),
    }];
    let used = text_width(&left);
    for hint in hint_tiers(state.level) {
        let hint_w = text_width(hint);
        if used + hint_w + 2 <= width {
            segs.push(Seg {
                text: " ".repeat(width - used - hint_w),
                style: SegStyle::plain(),
            });
            segs.push(Seg {
                text: hint.to_string(),
                style: SegStyle::fg(theme.text_muted),
            });
            break;
        }
    }
    Line { segs, image: None }
}

/// Build the page for one agent, reading the two sources X's communications
/// live in: its own transcript, and the logs of the rooms it is in.
///
/// A snapshot: taken once, when the page opens. Nothing here ticks.
pub fn snapshot(chat: &Chat, agent: &str) -> Dossier {
    let (history, stamps) = match chat.session.agents.view_of(agent) {
        Some((history, stamps, ..)) => (history, stamps),
        None => (Vec::new(), Vec::new()),
    };
    let rooms: Vec<(String, Vec<crate::channels::ChannelMessage>)> = chat
        .session
        .channels
        .rooms_of(agent)
        .into_iter()
        .map(|name| {
            let log = chat.session.channels.log_of(&name);
            (name, log)
        })
        .collect();
    dossier(agent, &history, &stamps, &rooms)
}

/// Open the page on the alternate screen. Mirrors
/// [`crate::tui::transcript::run_transcript_modal`], including the guarded
/// enter and the D77 panic-hook claim.
pub async fn run_perspective_modal(
    chat: &mut Chat,
    events: &mut EventStream,
    agent: &str,
    already_alt: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let _claim = (!already_alt).then(crate::tui::AltScreenClaim::enter);
    if !already_alt && let Err(e) = execute!(stdout(), EnterAlternateScreen, EnableMouseCapture) {
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        return Err(e.into());
    }
    let result = modal_loop(chat, events, agent).await;
    if !already_alt {
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
    result
}

fn viewport_of(height: usize) -> usize {
    height.saturating_sub(1).max(1)
}

async fn modal_loop(
    chat: &mut Chat,
    events: &mut EventStream,
    agent: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    let mut size = terminal.size()?;
    let mut width = size.width as usize;
    let theme = chat.theme.clone();
    let mut state = PerspectiveState::new(snapshot(chat, agent));
    // The page draws the conversation gutter, so the portraits have to reach
    // this screen too. All eight go once: the alternate screen is short-lived,
    // the payload is a few kilobytes, and knowing which faces a thread will
    // show would mean laying out every lane before drawing one.
    let cap = chat.image_cap;
    let pal = crate::tui::avatar::Palette::new(&theme);
    let pinned = chat.faces_pinned.clone();
    let gutter = crate::tui::avatar::Gutter::new(cap.is_some(), &pal, &pinned);
    let mut transmits = crate::tui::gfx::Transmits::default();

    loop {
        if let Some(cap) = cap {
            let faces: Vec<usize> = (0..crate::tui::avatar::COUNT).collect();
            let bytes = crate::tui::avatar::transmits(&faces, &cap, &mut transmits);
            crate::tui::term::write_transmits(terminal.backend_mut(), &bytes)?;
        }
        let visible = match state.level {
            Level::Index => index_rows(&state, width, &theme),
            Level::Thread => match &state.pager {
                Some(pager) => pager.visible_rows(&theme),
                None => Vec::new(),
            },
        };
        let bar = Row::new(footer(&state, width, &theme));
        terminal.draw(|frame| {
            let area = frame.area();
            let buf = frame.buffer_mut();
            view::render_rows(&visible, theme.text, buf, area);
            if area.height > 0 {
                let y = area.y + area.height - 1;
                let bar_area = ratatui::layout::Rect::new(area.x, y, area.width, 1);
                view::render_rows(std::slice::from_ref(&bar), theme.text, buf, bar_area);
            }
        })?;

        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                match on_key(&mut state, key.code, key.modifiers) {
                    Action::Close => break,
                    Action::Open => {
                        state.level = Level::Thread;
                        state.pager = Some(TranscriptState::new(
                            thread_rows(&state, width, &theme, Some(&gutter)),
                            viewport_of(size.height as usize),
                        ));
                    }
                    Action::Back => {
                        state.level = Level::Index;
                        state.pager = None;
                    }
                    Action::None => {}
                }
            }
            Some(Ok(Event::Paste(text))) => {
                if let Some(pager) = &mut state.pager
                    && let Some(input) = &mut pager.input
                {
                    input.push_str(&text);
                }
            }
            Some(Ok(Event::Mouse(mouse))) => {
                if let Some(pager) = &mut state.pager {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => pager.scroll(-3),
                        MouseEventKind::ScrollDown => pager.scroll(3),
                        _ => {}
                    }
                }
            }
            Some(Ok(Event::Resize(_, _))) => {
                size = terminal.size()?;
                width = size.width as usize;
                if state.level == Level::Thread {
                    let rows = thread_rows(&state, width, &theme, Some(&gutter));
                    if let Some(pager) = &mut state.pager {
                        pager.set_viewport(viewport_of(size.height as usize));
                        pager.set_rows(rows);
                    }
                }
            }
            Some(Ok(_)) => {}
            Some(Err(_)) | None => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{ContentBlock, Message, Role as ApiRole};
    use crate::channels::USER_NAME;
    use crate::tui::perspective::dossier;

    fn key(code: KeyCode) -> (KeyCode, KeyModifiers) {
        (code, KeyModifiers::NONE)
    }

    fn page() -> PerspectiveState {
        let history = vec![
            Message::user_text("map the parser"),
            Message::user_text("carry on"),
            Message {
                role: ApiRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: "done".to_string(),
                }],
            },
            Message::user_text(format!(
                "{}\nnice work",
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
        ];
        PerspectiveState::new(dossier("scout", &history, &[1, 2, 3, 4], &[]))
    }

    /// Enter walks in, Esc walks back out, and `q` leaves from either depth.
    #[test]
    fn enter_and_esc_walk_the_two_levels() {
        let mut state = page();
        assert_eq!(state.level, Level::Index);

        let (code, mods) = key(KeyCode::Enter);
        assert_eq!(on_key(&mut state, code, mods), Action::Open);
        // The loop is what actually opens it; the handler only says so.
        state.level = Level::Thread;
        state.pager = Some(TranscriptState::new(Vec::new(), 10));

        let (code, mods) = key(KeyCode::Esc);
        assert_eq!(
            on_key(&mut state, code, mods),
            Action::Back,
            "Esc in a thread returns to the index rather than closing the page"
        );
        state.level = Level::Index;
        state.pager = None;

        let (code, mods) = key(KeyCode::Esc);
        assert_eq!(
            on_key(&mut state, code, mods),
            Action::Close,
            "Esc at the index closes the page"
        );
    }

    #[test]
    fn q_closes_the_page_from_either_level() {
        let mut state = page();
        let (code, mods) = key(KeyCode::Char('q'));
        assert_eq!(on_key(&mut state, code, mods), Action::Close);

        state.level = Level::Thread;
        state.pager = Some(TranscriptState::new(Vec::new(), 10));
        let (code, mods) = key(KeyCode::Char('q'));
        assert_eq!(on_key(&mut state, code, mods), Action::Close);
    }

    /// While the search input is open it owns every key: `q` is a letter and
    /// Esc cancels the search, so neither may move a level.
    #[test]
    fn the_search_input_owns_q_and_esc() {
        let mut state = page();
        state.level = Level::Thread;
        let mut pager = TranscriptState::new(vec![Row::new(Line::plain("hello"))], 10);
        pager.open_search();
        state.pager = Some(pager);

        let (code, mods) = key(KeyCode::Char('q'));
        assert_eq!(on_key(&mut state, code, mods), Action::None);
        assert_eq!(
            state.pager.as_ref().and_then(|p| p.input.clone()),
            Some("q".to_string()),
            "it typed a letter instead of closing the page"
        );

        let (code, mods) = key(KeyCode::Esc);
        assert_eq!(on_key(&mut state, code, mods), Action::None);
        assert!(
            state.pager.as_ref().is_some_and(|p| p.input.is_none()),
            "Esc cancelled the search and stayed in the thread"
        );
    }

    /// The cursor walks lanes, not rows: it can never land on a group heading.
    #[test]
    fn the_cursor_walks_lanes_and_clamps() {
        let mut state = page();
        assert_eq!(state.selected, 0);
        let (code, mods) = key(KeyCode::Up);
        on_key(&mut state, code, mods);
        assert_eq!(state.selected, 0, "clamped at the top");
        for _ in 0..20 {
            let (code, mods) = key(KeyCode::Down);
            on_key(&mut state, code, mods);
        }
        assert_eq!(
            state.selected,
            state.page.lanes.len() - 1,
            "clamped at the bottom"
        );
        assert!(
            state.selected_lane().is_some(),
            "every cursor position is a real lane"
        );
    }

    /// A snapshot: the page is built once and does not change under the reader.
    #[test]
    fn the_page_is_a_snapshot() {
        let history = vec![Message::user_text("one")];
        let state = PerspectiveState::new(dossier("scout", &history, &[1], &[]));
        let before = state
            .page
            .lanes
            .iter()
            .find(|l| l.id == LaneId::Timeline)
            .map(|l| l.posts.len());

        // The domain moves on; the open page does not.
        let grown = vec![Message::user_text("one"), Message::user_text("two")];
        let _later = dossier("scout", &grown, &[1, 2], &[]);
        assert_eq!(
            state
                .page
                .lanes
                .iter()
                .find(|l| l.id == LaneId::Timeline)
                .map(|l| l.posts.len()),
            before,
            "an open page holds the snapshot it opened with"
        );
    }

    /// Read-only by construction: there is no key that submits, and no key the
    /// page handles that reaches a delivery path.
    #[test]
    fn no_key_on_the_page_can_send_anything() {
        let mut state = page();
        for code in [
            KeyCode::Enter,
            KeyCode::Char('a'),
            KeyCode::Char('i'),
            KeyCode::Backspace,
            KeyCode::Tab,
            KeyCode::Char('x'),
        ] {
            let action = on_key(&mut state, code, KeyModifiers::NONE);
            assert!(
                matches!(action, Action::None | Action::Open | Action::Close),
                "{code:?} produced {action:?}"
            );
            state.level = Level::Index;
            state.pager = None;
        }
    }

    /// The index says what each thread is, how much is in it and when it last
    /// moved — under the group headings the design names.
    #[test]
    fn the_index_lists_the_groups_with_counts() {
        let state = page();
        let theme = Theme::dark();
        let rows = index_rows(&state, 80, &theme);
        let text: Vec<String> = rows
            .iter()
            .map(|row| {
                row.line
                    .segs
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<String>()
            })
            .collect();
        let joined = text.join("\n");
        assert!(
            joined.contains("@scout · perspective · read-only"),
            "the page names itself and its stance: {joined}"
        );
        assert!(
            joined.contains("direct messages"),
            "the DM heading: {joined}"
        );
        assert!(joined.contains("intake"), "the intake heading: {joined}");
        assert!(joined.contains("@user"), "a counterpart row: {joined}");
        assert!(joined.contains("timeline"), "the merged lane: {joined}");
        assert!(
            joined.contains("1 message") || joined.contains("messages"),
            "counts are printed: {joined}"
        );
    }

    /// A thread's posts wear the conversation gutter — the same builder, so the
    /// page cannot drift from the DM view it is a dossier of.
    #[test]
    fn a_thread_wears_the_conversation_gutter() {
        let mut state = page();
        let user_lane = state
            .page
            .lanes
            .iter()
            .position(|l| l.id == LaneId::Dm(USER_NAME.to_string()))
            .expect("a user lane");
        state.selected = user_lane;
        state.level = Level::Thread;
        let theme = Theme::dark();
        let pal = crate::tui::avatar::Palette::new(&theme);
        let pinned = std::collections::HashMap::new();
        let gutter = crate::tui::avatar::Gutter::new(false, &pal, &pinned);

        let plain = thread_rows(&state, 80, &theme, None);
        let gutted = thread_rows(&state, 80, &theme, Some(&gutter));
        assert_eq!(
            plain.len(),
            gutted.len(),
            "a gutter indents rows, it does not add or drop any"
        );
        let width = gutter.width();
        let indented = gutted
            .iter()
            .skip(2)
            .filter(|row| !row.line.plain_text().trim().is_empty())
            .any(|row| {
                let text = row.line.plain_text();
                text.starts_with(&" ".repeat(width)) || text.starts_with(' ')
            });
        assert!(indented, "the posts moved right of the gutter");
    }

    /// A thread opens under its own rule, and the rule says read-only.
    #[test]
    fn a_thread_opens_under_a_read_only_rule() {
        let mut state = page();
        let user_lane = state
            .page
            .lanes
            .iter()
            .position(|l| l.id == LaneId::Dm(USER_NAME.to_string()))
            .expect("a user lane");
        state.selected = user_lane;
        state.level = Level::Thread;
        let theme = Theme::dark();
        let rows = thread_rows(&state, 80, &theme, None);
        let first: String = rows[0]
            .line
            .segs
            .iter()
            .map(|s| s.text.as_str())
            .collect::<String>();
        assert!(
            first.contains("@scout ↔ @user") && first.contains("read-only"),
            "the thread's rule names both sides and the stance: {first}"
        );
    }

    /// The footer tells the reader which keys exist at the depth they are at.
    #[test]
    fn the_footer_names_the_keys_of_the_level_it_is_on() {
        let mut state = page();
        let theme = Theme::dark();
        let index: String = footer(&state, 100, &theme)
            .segs
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert!(index.contains("enter open"), "{index}");
        assert!(index.contains("q close"), "{index}");

        state.level = Level::Thread;
        state.pager = Some(TranscriptState::new(Vec::new(), 10));
        let thread: String = footer(&state, 100, &theme)
            .segs
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert!(thread.contains("esc back"), "{thread}");
        assert!(thread.contains("search"), "{thread}");
    }
}
