//! Transcript view — the compensation for write-once scrollback (D82).
//!
//! The inline host prints settled rows straight into terminal scrollback and
//! never touches them again, so a collapsed tool output cannot be opened *in
//! place*: the row that would have to change is already the terminal's
//! property. Claude Code answers this with a transcript pager, and so does
//! this module — `ctrl+o` opens the whole session on the alternate screen,
//! where the rows can be rebuilt at will because nothing there is permanent.
//! `q` / `Esc` / `ctrl+o` puts the previous screen back untouched; that
//! restoration is the entire point of using the alternate screen instead of
//! reprinting into scrollback (which is what ctrl+o used to do, duplicating
//! the transcript on every press).
//!
//! Shape follows [`crate::tui::entity`]: a self-drawing modal loop that owns
//! the terminal for as long as it is open, with an `already_alt` flag so the
//! fullscreen host does not nest a second alternate screen inside its own.
//!
//! Split for testability: [`TranscriptState`] is pure state with pure
//! transitions (scroll / page / show-all / search), [`transcript_rows`] is the
//! row builder over the session, and [`modal_loop`] is the thin terminal shell
//! around them. Everything asserted below the `tests` module runs without a
//! terminal.

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

use crate::tui::chat::{Chat, Row};
use crate::tui::line::{Line, Seg, SegStyle, text_width};
use crate::tui::theme::Theme;
use crate::tui::{gfx, view};

/// One search hit: a byte range inside a row's plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// Row index in [`TranscriptState::rows`].
    pub row: usize,
    /// Start byte offset in the row's plain text.
    pub start: usize,
    /// End byte offset (exclusive).
    pub end: usize,
}

/// What the modal loop owes the caller after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing beyond a repaint.
    None,
    /// The presentation changed: rebuild the rows at the current `show_all`.
    Rebuild,
    /// Open a content image in the desktop's viewer (D97). The pager cannot
    /// spawn anything — it has no `Chat` and no terminal — so it names the row
    /// and the loop, which has both, does the opening.
    OpenImage(usize),
    /// Close the view.
    Close,
}

/// The pager's whole state. No terminal, no session — the rows arrive already
/// laid out for a width, and every transition here is a function of this
/// struct alone.
#[derive(Debug, Clone)]
pub struct TranscriptState {
    /// Every row of the session, in order.
    pub rows: Vec<Row>,
    /// Index of the topmost visible row.
    pub offset: usize,
    /// Visible row count (terminal height minus the footer).
    pub viewport: usize,
    /// Expand every collapsible block. Default on: the user opened the
    /// transcript to see what the fold hid.
    pub show_all: bool,
    /// Text being typed into the `/` search input (None = the input is closed).
    pub input: Option<String>,
    /// Committed query (empty = no search).
    pub query: String,
    /// Hits of the committed query, in row order.
    pub matches: Vec<Match>,
    /// Index into `matches` of the hit `n`/`N` are sitting on.
    pub current: usize,
    /// Scroll offset saved when the search input opened, restored on cancel.
    saved_offset: usize,
}

impl TranscriptState {
    /// A pager over `rows` with `viewport` visible rows, parked at the bottom:
    /// the session's most recent rows are what the user was just looking at.
    pub fn new(rows: Vec<Row>, viewport: usize) -> Self {
        let mut state = Self {
            rows,
            offset: 0,
            viewport: viewport.max(1),
            show_all: true,
            input: None,
            query: String::new(),
            matches: Vec::new(),
            current: 0,
            saved_offset: 0,
        };
        state.bottom();
        state
    }

    /// Largest legal `offset`: the last screenful, never past it.
    pub fn max_offset(&self) -> usize {
        self.rows.len().saturating_sub(self.viewport)
    }

    fn clamp(&mut self) {
        self.offset = self.offset.min(self.max_offset());
    }

    /// Scroll by `delta` rows (negative = towards the top), clamped at both ends.
    pub fn scroll(&mut self, delta: isize) {
        let offset = if delta < 0 {
            self.offset.saturating_sub(delta.unsigned_abs())
        } else {
            self.offset.saturating_add(delta as usize)
        };
        self.offset = offset.min(self.max_offset());
    }

    /// Scroll by `delta` screenfuls. One row of overlap is kept so the reader
    /// can stitch the pages together.
    pub fn page(&mut self, delta: isize) {
        let step = self.viewport.saturating_sub(1).max(1);
        self.scroll(delta.saturating_mul(step as isize));
    }

    /// Jump to the first row.
    pub fn top(&mut self) {
        self.offset = 0;
    }

    /// Jump to the last screenful.
    pub fn bottom(&mut self) {
        self.offset = self.max_offset();
    }

    /// Resize the viewport (terminal resize), keeping the top row where it is.
    pub fn set_viewport(&mut self, viewport: usize) {
        self.viewport = viewport.max(1);
        self.clamp();
    }

    /// Replace the rows, keeping the reading position *proportionally* — the
    /// only anchor that survives a presentation change, since expanding every
    /// fold moves every row number below the first one. Search hits are
    /// recomputed against the new rows.
    pub fn set_rows(&mut self, rows: Vec<Row>) {
        let anchor = match self.rows.len() {
            0 => 0,
            old => self.offset.saturating_mul(rows.len()) / old,
        };
        self.rows = rows;
        self.offset = anchor;
        self.clamp();
        self.recompute_matches();
    }

    /// ctrl+e: flip the presentation. The caller rebuilds the rows and hands
    /// them back through [`TranscriptState::set_rows`].
    pub fn toggle_show_all(&mut self) -> bool {
        self.show_all = !self.show_all;
        self.show_all
    }

    /// `/`: open the search input, remembering where the reader was.
    pub fn open_search(&mut self) {
        self.saved_offset = self.offset;
        self.input = Some(String::new());
    }

    /// Esc inside the input: close it and put the reading position back. The
    /// previously committed query, if any, stays committed — Esc cancels the
    /// typing, not the search that was already running.
    pub fn cancel_search(&mut self) {
        self.input = None;
        self.offset = self.saved_offset;
        self.clamp();
    }

    /// Enter inside the input: commit the query and jump to the first hit at
    /// or after the current position (wrapping to the top when there is none).
    pub fn commit_search(&mut self) {
        let Some(query) = self.input.take() else {
            return;
        };
        self.query = query;
        self.recompute_matches();
        if self.matches.is_empty() {
            return;
        }
        self.current = self
            .matches
            .iter()
            .position(|m| m.row >= self.offset)
            .unwrap_or(0);
        self.reveal_current();
    }

    /// Recompute the hits of the committed query against the current rows.
    fn recompute_matches(&mut self) {
        self.matches.clear();
        self.current = 0;
        if self.query.is_empty() {
            return;
        }
        let needle = fold(&self.query);
        if needle.is_empty() {
            return;
        }
        for (row, item) in self.rows.iter().enumerate() {
            for (start, end) in find_all(&item.line.plain_text(), &needle) {
                self.matches.push(Match { row, start, end });
            }
        }
    }

    /// `n` / `N`: step to the next / previous hit, wrapping at both ends.
    pub fn step_match(&mut self, forward: bool) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() - 1;
        self.current = if forward {
            if self.current >= last {
                0
            } else {
                self.current + 1
            }
        } else if self.current == 0 {
            last
        } else {
            self.current - 1
        };
        self.reveal_current();
    }

    /// Scroll so the current hit is on screen; already-visible hits do not move
    /// the view (a jump per keystroke makes the surrounding context unreadable).
    fn reveal_current(&mut self) {
        let Some(hit) = self.matches.get(self.current) else {
            return;
        };
        if hit.row >= self.offset && hit.row < self.offset + self.viewport {
            return;
        }
        self.offset = hit.row.saturating_sub(self.viewport / 2);
        self.clamp();
    }

    /// The visible rows, with search hits highlighted.
    pub fn visible_rows(&self, theme: &Theme) -> Vec<Row> {
        let end = (self.offset + self.viewport).min(self.rows.len());
        let current = self.matches.get(self.current).copied();
        self.rows[self.offset.min(end)..end]
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let index = self.offset + i;
                let hits: Vec<Match> = self
                    .matches
                    .iter()
                    .copied()
                    .filter(|m| m.row == index)
                    .collect();
                if hits.is_empty() {
                    return row.clone();
                }
                let mut out = row.clone();
                out.line = highlight(&row.line, &hits, current, theme);
                out
            })
            .collect()
    }

    /// The first row on screen that carries a picture, if any. What `o` acts
    /// on: a pager has no selection, and the top of the window is where the
    /// reader's eye already is.
    pub fn first_visible_image(&self) -> Option<usize> {
        let end = (self.offset + self.viewport).min(self.rows.len());
        (self.offset.min(end)..end).find(|i| {
            self.rows
                .get(*i)
                .is_some_and(|row| row.line.image.is_some())
        })
    }

    /// Progress through the document, 0–100.
    pub fn percent(&self) -> usize {
        let max = self.max_offset();
        if max == 0 {
            return 100;
        }
        self.offset * 100 / max
    }
}

/// Case-folded characters of `text`, one per source character (`char::to_lowercase`
/// can yield several — taking the first keeps the mapping index-for-index, which
/// is what lets a hit found in the folded text be reported as a byte range of the
/// original).
fn fold(text: &str) -> Vec<char> {
    text.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

/// Every occurrence of the folded `needle` in `hay`, as byte ranges of `hay`.
fn find_all(hay: &str, needle: &[char]) -> Vec<(usize, usize)> {
    let chars: Vec<(usize, char)> = hay.char_indices().collect();
    if needle.is_empty() || chars.len() < needle.len() {
        return Vec::new();
    }
    let folded: Vec<char> = chars
        .iter()
        .map(|(_, c)| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= folded.len() {
        if folded[i..i + needle.len()] == *needle {
            let (start, _) = chars[i];
            let (last_at, last) = chars[i + needle.len() - 1];
            out.push((start, last_at + last.len_utf8()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Repaint `line` with the hit ranges given a background. The segment list is
/// split at range boundaries; nothing else about the line changes, so a diff
/// row keeps its diff colours under the highlight.
fn highlight(line: &Line, hits: &[Match], current: Option<Match>, theme: &Theme) -> Line {
    // An image row is placeholder cells addressing a picture, not text: splitting
    // its segments would corrupt the addressing.
    if line.image.is_some() {
        return line.clone();
    }
    let mut segs: Vec<Seg> = Vec::new();
    let mut at = 0usize;
    for seg in &line.segs {
        let seg_end = at + seg.text.len();
        let mut cut = at;
        // Boundaries of this segment that a hit starts or ends on, in order.
        let mut points: Vec<usize> = Vec::new();
        for hit in hits {
            for edge in [hit.start, hit.end] {
                if edge > at && edge < seg_end {
                    points.push(edge);
                }
            }
        }
        points.sort_unstable();
        points.dedup();
        points.push(seg_end);
        for point in points {
            let Some(text) = seg.text.get(cut - at..point - at) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            let hit = hits.iter().find(|h| h.start <= cut && cut < h.end);
            let style = match hit {
                Some(h) if current == Some(*h) => seg
                    .style
                    .patch(SegStyle::fg(on_accent(theme)).with_bg(theme.claude).bold()),
                Some(_) => seg
                    .style
                    .patch(SegStyle::plain().with_bg(theme.code_block_bg)),
                None => seg.style,
            };
            segs.push(Seg {
                text: text.to_string(),
                style,
            });
            cut = point;
        }
        at = seg_end;
    }
    Line { segs, image: None }
}

/// Foreground for text sitting on the accent highlight. Spelled in RGB rather
/// than a named ANSI colour, which the palette does not control (D92's rule for
/// the rendering layer).
fn on_accent(theme: &Theme) -> ratatui::style::Color {
    if theme.is_dark {
        ratatui::style::Color::Rgb(26, 26, 26)
    } else {
        ratatui::style::Color::Rgb(255, 255, 255)
    }
}

/// Key hints, longest set first: the bar picks the widest one that fits whole,
/// so a narrow terminal loses a hint rather than half a word. `q close` and the
/// presentation toggle survive to the last tier — they are the two keys a reader
/// who wandered in cannot guess.
fn hint_tiers(show_all: bool) -> [String; 3] {
    let expand = if show_all {
        "ctrl+e collapse"
    } else {
        "ctrl+e expand all"
    };
    [
        format!("j/k scroll · g/G ends · / search · n/N hits · o image · {expand} · q close"),
        format!("j/k scroll · / search · o image · {expand} · q close"),
        format!("{expand} · q close"),
    ]
}

/// The one-line hint bar under the pager.
pub fn footer(state: &TranscriptState, width: usize, theme: &Theme) -> Line {
    let mut line = Line::empty();
    let mut used = 0usize;
    // Truncation is the last resort (a terminal narrower than any hint tier);
    // everything above chooses a shorter phrasing instead.
    let push = |text: String, style: SegStyle, line: &mut Line, used: &mut usize| {
        let room = width.saturating_sub(*used);
        if room == 0 {
            return;
        }
        let text = if text_width(&text) > room {
            text.chars()
                .scan(0usize, |w, c| {
                    *w += text_width(&c.to_string());
                    (*w <= room).then_some(c)
                })
                .collect()
        } else {
            text
        };
        *used += text_width(&text);
        line.push_styled(text, style);
    };
    if let Some(input) = &state.input {
        push(
            format!("/{input}▌"),
            SegStyle::fg(theme.text),
            &mut line,
            &mut used,
        );
        push(
            "  enter search · esc cancel".to_string(),
            theme.muted(),
            &mut line,
            &mut used,
        );
        return line;
    }
    push(
        "transcript".to_string(),
        SegStyle::fg(theme.claude).bold(),
        &mut line,
        &mut used,
    );
    // A transcript that fits on one screen has no position to report.
    if state.max_offset() > 0 {
        push(
            format!(" {}%", state.percent()),
            theme.muted(),
            &mut line,
            &mut used,
        );
    }
    if !state.query.is_empty() {
        let (hits, style) = if state.matches.is_empty() {
            (" · no match".to_string(), SegStyle::fg(theme.warning))
        } else {
            (
                format!(" · {}/{} matches", state.current + 1, state.matches.len()),
                SegStyle::fg(theme.claude),
            )
        };
        push(hits, style, &mut line, &mut used);
    }
    let room = width.saturating_sub(used).saturating_sub(3);
    let hints = hint_tiers(state.show_all);
    let hint = hints
        .iter()
        .find(|h| text_width(h) <= room)
        .or(hints.last());
    if let Some(hint) = hint {
        push(format!(" · {hint}"), theme.muted(), &mut line, &mut used);
    }
    line
}

/// One key inside the view. The search input owns every key while it is open —
/// a `q` typed into a query is a letter, not an exit.
pub fn on_key(state: &mut TranscriptState, code: KeyCode, modifiers: KeyModifiers) -> Action {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    if state.input.is_some() {
        match code {
            KeyCode::Esc => state.cancel_search(),
            KeyCode::Enter => state.commit_search(),
            KeyCode::Backspace => {
                if let Some(input) = &mut state.input {
                    input.pop();
                }
            }
            KeyCode::Char('c') if ctrl => state.cancel_search(),
            KeyCode::Char(c) if !ctrl && !modifiers.contains(KeyModifiers::ALT) => {
                if let Some(input) = &mut state.input {
                    input.push(c);
                }
            }
            _ => {}
        }
        return Action::None;
    }
    match code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Close,
        KeyCode::Char('o' | 'c') if ctrl => Action::Close,
        // `o` opens the picture in view. The pager has no cursor to sit on a
        // row with, so "the image row" is the first one on screen — which is
        // the one the reader stopped scrolling at.
        KeyCode::Char('o') => match state.first_visible_image() {
            Some(row) => Action::OpenImage(row),
            None => Action::None,
        },
        KeyCode::Char('e') if ctrl => {
            state.toggle_show_all();
            Action::Rebuild
        }
        KeyCode::Char('j') | KeyCode::Down => {
            state.scroll(1);
            Action::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.scroll(-1);
            Action::None
        }
        KeyCode::Char(' ') | KeyCode::PageDown => {
            state.page(1);
            Action::None
        }
        KeyCode::PageUp => {
            state.page(-1);
            Action::None
        }
        KeyCode::Char('g') | KeyCode::Home => {
            state.top();
            Action::None
        }
        KeyCode::Char('G') | KeyCode::End => {
            state.bottom();
            Action::None
        }
        KeyCode::Char('/') => {
            state.open_search();
            Action::None
        }
        KeyCode::Char('n') => {
            state.step_match(true);
            Action::None
        }
        KeyCode::Char('N') => {
            state.step_match(false);
            Action::None
        }
        _ => Action::None,
    }
}

/// Fold state of the whole transcript, so the pager can force everything open
/// and hand the main view back exactly what it had.
struct Folds {
    activities: Vec<Vec<(bool, bool)>>,
    groups: Vec<Vec<bool>>,
    flushed_segments: usize,
    tail_start: usize,
    mark_base: usize,
}

/// Build every row of the session at `width`.
///
/// The row source is [`Chat::build_rows`] itself — the same builder the two
/// hosts draw from, so markdown, diffs, images, CJK wrapping and the collapse
/// summaries are rendered once, here as everywhere. Two things are borrowed for
/// the duration of the build and given back: the flush cursor (rewound to zero,
/// so the document covers the whole session and not just the unflushed tail)
/// and the fold state (forced open when `show_all`). `dirty` is set on the way
/// out so the host rebuilds its own document before drawing again.
/// The messages the pager is looking at. `Chat::build_rows` already swaps an
/// away page's own document in for the length of one build, so the pager has
/// always *shown* the page on screen — but the fold bookkeeping around it read
/// `Chat::messages`, which by construction always describes main. On a page,
/// that made `a` (expand everything) a dead key: it opened main's folds, and
/// main's document was not the one being drawn.
fn active_messages(chat: &mut Chat) -> &mut Vec<crate::tui::chat::UiMessage> {
    match chat.away.as_mut() {
        Some(page) => &mut page.messages,
        None => &mut chat.messages,
    }
}

pub fn transcript_rows(chat: &mut Chat, width: usize, show_all: bool) -> Vec<Row> {
    let (activities, groups) = {
        let messages = active_messages(chat);
        (
            messages
                .iter()
                .map(|m| {
                    m.activities
                        .iter()
                        .map(|a| (a.expanded, a.auto_expanded))
                        .collect()
                })
                .collect(),
            messages
                .iter()
                .map(|m| m.groups.iter().map(|g| g.expanded).collect())
                .collect(),
        )
    };
    let saved = Folds {
        activities,
        groups,
        flushed_segments: chat.flushed_segments,
        tail_start: chat.tail_start,
        mark_base: chat.mark_base,
    };
    chat.flushed_segments = 0;
    chat.tail_start = 0;
    chat.mark_base = 0;
    // CC's `isTranscriptMode`: a row that keeps a body out of the flow may show
    // it here. Restored below with the fold state, so the inline document is
    // built without it and nothing it already flushed can disagree.
    let saved_transcript_mode = chat.transcript_mode;
    chat.transcript_mode = true;
    if show_all {
        for message in active_messages(chat) {
            for act in &mut message.activities {
                act.expanded = true;
            }
            for group in &mut message.groups {
                group.expanded = true;
            }
        }
    }
    let rows = chat.build_rows(width).rows.clone();
    for (message, folds) in active_messages(chat).iter_mut().zip(&saved.activities) {
        for (act, (expanded, auto)) in message.activities.iter_mut().zip(folds) {
            act.expanded = *expanded;
            act.auto_expanded = *auto;
        }
    }
    for (message, folds) in active_messages(chat).iter_mut().zip(&saved.groups) {
        for (group, expanded) in message.groups.iter_mut().zip(folds) {
            group.expanded = *expanded;
        }
    }
    chat.flushed_segments = saved.flushed_segments;
    chat.tail_start = saved.tail_start;
    chat.mark_base = saved.mark_base;
    chat.transcript_mode = saved_transcript_mode;
    chat.dirty = true;
    rows
}

/// Open the transcript view. `already_alt`: the fullscreen host is already on
/// the alternate screen, so there is no nested enter/leave.
///
/// Entering is guarded in both directions — a failure halfway leaves the
/// alternate screen and mouse capture behind rather than stranding the user in
/// them — and while the inline host is on the alternate screen the D77 panic
/// hook is told which teardown it owes ([`crate::tui::AltScreenClaim`]).
pub async fn run_transcript_modal(
    chat: &mut Chat,
    events: &mut EventStream,
    already_alt: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let _claim = (!already_alt).then(crate::tui::AltScreenClaim::enter);
    if !already_alt && let Err(e) = execute!(stdout(), EnterAlternateScreen, EnableMouseCapture) {
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        return Err(e.into());
    }
    let result = modal_loop(chat, events).await;
    if !already_alt {
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
    result
}

/// Viewport rows for a terminal of `height` rows: everything but the footer.
fn viewport_of(height: usize) -> usize {
    height.saturating_sub(1).max(1)
}

async fn modal_loop(
    chat: &mut Chat,
    events: &mut EventStream,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    let mut size = terminal.size()?;
    let mut width = size.width as usize;
    let mut state = TranscriptState::new(
        transcript_rows(chat, width, true),
        viewport_of(size.height as usize),
    );
    // The images the rows address were transmitted on the main screen; the
    // terminal's image store is not per-screen, but a resize can purge it, so
    // the bookkeeping starts empty and the visible rows are re-sent once.
    let mut transmits = gfx::Transmits::default();
    let theme = chat.theme.clone();

    loop {
        let visible = state.visible_rows(&theme);
        if let Some(cap) = chat.image_cap {
            let bytes =
                crate::tui::app::image_transmits(cap, &chat.images, &visible, &mut transmits);
            crate::tui::term::write_transmits(terminal.backend_mut(), &bytes)?;
        }
        // The hint bar owns the last row and the content is sized to the rest, so
        // the two never write the same cell.
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
                    Action::Rebuild => {
                        let rows = transcript_rows(chat, width, state.show_all);
                        state.set_rows(rows);
                    }
                    // The viewer is spawned detached, so the pager keeps the
                    // terminal and the next key lands here as usual.
                    Action::OpenImage(row) => {
                        if let Some(id) = state.rows.get(row).and_then(|r| chat.image_at_row(r)) {
                            chat.open_image(id);
                        }
                    }
                    Action::None => {}
                }
            }
            Some(Ok(Event::Paste(text))) => {
                if let Some(input) = &mut state.input {
                    input.push_str(&text);
                }
            }
            Some(Ok(Event::Mouse(mouse))) => match mouse.kind {
                MouseEventKind::ScrollUp => state.scroll(-3),
                MouseEventKind::ScrollDown => state.scroll(3),
                _ => {}
            },
            Some(Ok(Event::Resize(_, _))) => {
                size = terminal.size()?;
                width = size.width as usize;
                state.set_viewport(viewport_of(size.height as usize));
                let rows = transcript_rows(chat, width, state.show_all);
                state.set_rows(rows);
                transmits.reset();
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
    use crate::tui::chat::{Chat, Role, UiMessage};
    use crate::tui::test_util::chat_at;
    use crate::ui::UiEvent;
    use serde_json::json;

    fn message(role: Role, text: &str) -> UiMessage {
        UiMessage {
            speaker: None,
            role,
            text: text.to_string(),
            at: 0,
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        }
    }

    fn send(chat: &mut Chat, event: UiEvent) {
        let _ = chat.events.send(event);
        chat.drain_events();
    }

    fn rows_of(texts: &[&str]) -> Vec<Row> {
        texts.iter().map(|t| Row::new(Line::plain(*t))).collect()
    }

    fn state_of(texts: &[&str], viewport: usize) -> TranscriptState {
        TranscriptState::new(rows_of(texts), viewport)
    }

    fn texts(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| r.line.plain_text()).collect()
    }

    /// A finished tool call whose output is long enough to be worth folding.
    fn tool_call(chat: &mut Chat, name: &str, output: &str) {
        send(chat, UiEvent::ToolStart { name: name.into() });
        send(
            chat,
            UiEvent::ToolReady {
                tool_call_id: "t1".into(),
                name: name.into(),
                input: json!({ "file_path": "notes.md" }),
                standalone: false,
            },
        );
        send(
            chat,
            UiEvent::ToolDone(crate::query::ToolCallDone {
                tool_call_id: "t1".into(),
                name: name.into(),
                summary: format!("{name} 3 lines"),
                output: output.into(),
                status: crate::query::ToolCallStatus::Done,
                duration_ms: 12,
                diff: None,
            }),
        );
    }

    /// The row builder is the compensation itself: with show_all off the pager
    /// shows what the inline screen showed (a summary and its hint), with
    /// show_all on it shows the output that fold was hiding.
    #[test]
    fn show_all_reveals_folded_tool_output() {
        let mut chat = chat_at(100, 30);
        chat.messages.push(message(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        tool_call(&mut chat, "Read", "alpha line\nbeta line\ngamma line");

        let collapsed = texts(&transcript_rows(&mut chat, 100, false)).join("\n");
        let expanded = texts(&transcript_rows(&mut chat, 100, true)).join("\n");
        assert!(
            !collapsed.contains("alpha line"),
            "collapsed keeps the output folded: {collapsed}"
        );
        assert!(
            collapsed.contains("ctrl+o to expand"),
            "collapsed still advertises the way in: {collapsed}"
        );
        assert!(
            expanded.contains("alpha line") && expanded.contains("gamma line"),
            "show_all opens the fold: {expanded}"
        );
    }

    /// Thinking blocks fold on the same mechanism and must open on the same key.
    #[test]
    fn show_all_reveals_thinking_blocks() {
        let mut chat = chat_at(100, 30);
        chat.messages.push(message(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        send(
            &mut chat,
            UiEvent::ThinkingDelta("weighing the tradeoff".into()),
        );
        // Text after thinking closes the block, which is what makes it collapsible.
        send(&mut chat, UiEvent::TextDelta("so: yes.".into()));

        let collapsed = texts(&transcript_rows(&mut chat, 100, false)).join("\n");
        let expanded = texts(&transcript_rows(&mut chat, 100, true)).join("\n");
        assert!(
            !collapsed.contains("weighing the tradeoff"),
            "collapsed hides the reasoning: {collapsed}"
        );
        assert!(
            expanded.contains("weighing the tradeoff"),
            "show_all shows it: {expanded}"
        );
    }

    /// Building the transcript borrows the session's fold state and flush
    /// cursor; leaving it borrowed would collapse the main view's open rows and
    /// reprint the whole session into scrollback on the next frame.
    #[test]
    fn building_the_transcript_leaves_the_session_untouched() {
        let mut chat = chat_at(100, 30);
        chat.messages.push(message(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        tool_call(&mut chat, "Read", "alpha line\nbeta line");
        chat.flushed_segments = 1;
        chat.tail_start = 4;
        chat.mark_base = 1;
        let before: Vec<bool> = chat.messages[0]
            .activities
            .iter()
            .map(|a| a.expanded)
            .collect();

        let _ = transcript_rows(&mut chat, 100, true);

        let after: Vec<bool> = chat.messages[0]
            .activities
            .iter()
            .map(|a| a.expanded)
            .collect();
        assert_eq!(before, after, "fold state is given back");
        assert_eq!(chat.flushed_segments, 1, "flush cursor is given back");
        assert_eq!(chat.tail_start, 4);
        assert_eq!(chat.mark_base, 1);
        assert!(chat.dirty, "the host rebuilds its own document next frame");
    }

    #[test]
    fn scrolling_clamps_at_both_ends() {
        let mut state = state_of(&["a", "b", "c", "d", "e", "f"], 3);
        assert_eq!(state.offset, 3, "opens at the bottom");
        state.scroll(9);
        assert_eq!(state.offset, 3, "never past the last screenful");
        state.scroll(-99);
        assert_eq!(state.offset, 0, "never above the first row");
        state.page(1);
        assert_eq!(state.offset, 2, "a page keeps one row of overlap");
        state.page(-1);
        assert_eq!(state.offset, 0);
        state.bottom();
        assert_eq!(state.offset, 3);
        state.top();
        assert_eq!(state.offset, 0);
    }

    /// A document shorter than the viewport has nowhere to scroll.
    #[test]
    fn a_short_document_never_scrolls() {
        let mut state = state_of(&["only"], 10);
        assert_eq!(state.max_offset(), 0);
        state.page(1);
        state.scroll(5);
        assert_eq!(state.offset, 0);
        assert_eq!(state.percent(), 100);
    }

    #[test]
    fn search_cycles_matches_in_order_and_wraps() {
        let mut state = state_of(&["alpha", "beta", "alpha again", "gamma"], 2);
        state.open_search();
        for c in "alpha".chars() {
            on_key(&mut state, KeyCode::Char(c), KeyModifiers::NONE);
        }
        on_key(&mut state, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            state.matches.iter().map(|m| m.row).collect::<Vec<_>>(),
            vec![0, 2],
            "hits come out in row order"
        );
        assert_eq!(
            state.matches[0],
            Match {
                row: 0,
                start: 0,
                end: 5
            }
        );
        // The reader is parked on rows 2-3, so the search lands on the hit at or
        // after that, not back at the top.
        assert_eq!(state.current, 1);
        state.step_match(true);
        assert_eq!(state.current, 0, "next wraps");
        state.step_match(true);
        assert_eq!(state.current, 1);
        state.step_match(false);
        assert_eq!(state.current, 0, "prev wraps the other way");

        // A query with no hit leaves the cycle empty rather than jumping anywhere.
        state.query = "nowhere".into();
        state.recompute_matches();
        state.step_match(true);
        assert!(state.matches.is_empty());
        assert_eq!(state.current, 0);
    }

    /// Search is case-insensitive, and the byte range it reports addresses the
    /// original text (not the folded copy).
    #[test]
    fn search_is_case_insensitive_with_original_offsets() {
        let mut state = state_of(&["Ünicode ALPHA here"], 4);
        state.query = "alpha".into();
        state.recompute_matches();
        assert_eq!(state.matches.len(), 1);
        let hit = state.matches[0];
        let text = state.rows[0].line.plain_text();
        assert_eq!(&text[hit.start..hit.end], "ALPHA");
    }

    #[test]
    fn escape_cancels_the_search_input_and_restores_the_position() {
        let mut state = state_of(&["a", "b", "c", "d", "e", "f"], 2);
        state.scroll(-2);
        let before = state.offset;
        on_key(&mut state, KeyCode::Char('/'), KeyModifiers::NONE);
        for c in "b".chars() {
            on_key(&mut state, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(state.input.as_deref(), Some("b"));
        on_key(&mut state, KeyCode::Esc, KeyModifiers::NONE);
        assert!(state.input.is_none(), "the input closes");
        assert_eq!(state.offset, before, "the reading position is intact");
        assert!(state.query.is_empty(), "nothing was committed");
    }

    /// While the input is open every key is text: `q` does not close the view.
    #[test]
    fn the_search_input_owns_its_keys() {
        let mut state = state_of(&["alpha", "beta"], 2);
        on_key(&mut state, KeyCode::Char('/'), KeyModifiers::NONE);
        assert_eq!(
            on_key(&mut state, KeyCode::Char('q'), KeyModifiers::NONE),
            Action::None
        );
        assert_eq!(state.input.as_deref(), Some("q"));
        on_key(&mut state, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(state.input.as_deref(), Some(""));
    }

    /// ctrl+e asks for a rebuild and the new rows keep the reading position
    /// proportionally — the row numbers all moved, the place in the session did not.
    #[test]
    fn toggling_show_all_rebuilds_and_reanchors() {
        let mut state = state_of(&["a", "b", "c", "d", "e", "f", "g", "h"], 2);
        state.offset = 4;
        assert_eq!(
            on_key(&mut state, KeyCode::Char('e'), KeyModifiers::CONTROL),
            Action::Rebuild
        );
        assert!(!state.show_all);
        state.set_rows(rows_of(&["a", "b", "c", "d"]));
        assert_eq!(state.offset, 2, "half way through, still half way through");
        state.set_rows(Vec::new());
        assert_eq!(
            state.offset, 0,
            "an empty document clamps rather than panics"
        );
    }

    #[test]
    fn q_esc_and_ctrl_o_all_close() {
        for (code, mods) in [
            (KeyCode::Char('q'), KeyModifiers::NONE),
            (KeyCode::Esc, KeyModifiers::NONE),
            (KeyCode::Char('o'), KeyModifiers::CONTROL),
        ] {
            let mut state = state_of(&["a"], 2);
            assert_eq!(on_key(&mut state, code, mods), Action::Close, "{code:?}");
        }
    }

    /// The highlight splits segments on the hit boundaries and paints only
    /// those — the rest of the row keeps the colours the renderer gave it.
    #[test]
    fn highlight_covers_exactly_the_match() {
        let theme = Theme::dark();
        let line = Line::plain("find the needle here");
        let hits = [Match {
            row: 0,
            start: 9,
            end: 15,
        }];
        let out = highlight(&line, &hits, Some(hits[0]), &theme);
        assert_eq!(out.plain_text(), "find the needle here");
        let painted: String = out
            .segs
            .iter()
            .filter(|s| s.style.bg.is_some())
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(painted, "needle");
    }

    /// Wide characters are wrapped by the shared row builder, so no row of the
    /// transcript is ever wider than the terminal and no wide char is split.
    #[test]
    fn wide_rows_wrap_within_the_width() {
        let mut chat = chat_at(40, 30);
        chat.messages
            .push(message(Role::User, &"北海啊要多想".repeat(12)));
        for row in transcript_rows(&mut chat, 40, true) {
            let text = row.line.plain_text();
            assert!(
                text_width(&text) <= 40,
                "row wider than the terminal: {text:?}"
            );
        }
    }

    /// The whole session, not just the tail: a flushed prefix is still in the
    /// pager, which is what makes the view a compensation for write-once
    /// scrollback rather than a second copy of the viewport.
    #[test]
    fn the_pager_covers_the_flushed_prefix() {
        let mut chat = chat_at(80, 30);
        chat.messages.push(message(Role::User, "first question"));
        chat.messages.push(message(Role::Assistant, "an answer"));
        chat.flushed_segments = 2;
        let all = texts(&transcript_rows(&mut chat, 80, true)).join("\n");
        assert!(all.contains("first question"), "{all}");
        assert!(all.contains("an answer"), "{all}");
    }

    /// The footer states where the reader is and what the keys do; in search it
    /// shows the query being typed instead.
    #[test]
    fn the_footer_reports_position_and_search() {
        let theme = Theme::dark();
        let mut state = state_of(&["a", "b", "c", "d"], 2);
        let bar = footer(&state, 100, &theme).plain_text();
        assert!(bar.starts_with("transcript 100%"), "{bar}");
        assert!(
            bar.contains("ctrl+e collapse") && bar.contains("q close"),
            "{bar}"
        );
        state.open_search();
        if let Some(input) = &mut state.input {
            input.push_str("需要");
        }
        let bar = footer(&state, 100, &theme).plain_text();
        assert!(bar.starts_with("/需要▌"), "{bar}");
        assert!(bar.contains("esc cancel"), "{bar}");
        // Narrow terminals truncate rather than overflow the row.
        let narrow = footer(&state, 6, &theme).plain_text();
        assert!(text_width(&narrow) <= 6, "{narrow:?}");
    }
}
