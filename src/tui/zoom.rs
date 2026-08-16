//! The zoomed view (D105): the screen, temporarily, is one agent's own
//! conversation — and the composer is talking to it.
//!
//! CC's answer to many agents in one terminal is one transcript plus a status
//! layer plus a view you can *zoom into* and back out of
//! (`hooks/useBackgroundTaskNavigation.ts`, `components/TeammateViewHeader.tsx`).
//! D103 built the transcript, D104 the status layer; this is the zoom. It is
//! not a conversation you switch to — nothing is spliced, nothing replays into
//! the flow, and the transcript underneath is exactly where you left it.
//!
//! **Why the alternate screen.** The inline host writes each settled row into
//! the terminal's scrollback once and never touches it again
//! ([`crate::tui::term`]); a view that replaces the body cannot exist there.
//! So the zoom takes the road the transcript pager (D82) takes: a self-driving
//! alt-screen loop that owns the terminal while it is open, with the same
//! guarded enter and the same D77 panic-hook claim.
//! Leaving the alternate screen restores the primary one — with its scrollback
//! — byte for byte.
//!
//! **What it writes into the host, and what it must not.** Everything on this
//! screen is derived from the *domain* — the agent registry, the room log —
//! read fresh on every frame. The only [`Chat`] state the loop touches is the
//! composer draft, the tree's cursor, the zoom pointer itself, the accounting
//! store's active conversation and the frame counter. It never calls
//! [`Chat::build_rows`], never pushes a [`crate::tui::chat::UiMessage`], never
//! drains an event channel and never moves `flushed_segments`. So the messages
//! that arrive while it is open arrive *after* it closes, in one batch, in
//! order — exactly as if the zoom had never happened, which is the property the
//! round-trip test pins.
//!
//! **Two things it is over.** An agent — the ordinary case, entered from the
//! tree or from the background dialog's `f` — and a room, which is bingo's own
//! extension and is reached from that dialog's Rooms section (D107).

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

use crate::agents::AgentState;
use crate::channels::USER_NAME;
use crate::tui::buffer::{BufferId, Post, channel_posts, zoom_posts};
use crate::tui::bufferview::{DirectTarget, sender_runs};
use crate::tui::chat::{Chat, Row, one_line};
use crate::tui::line::{Line, SegStyle};
use crate::tui::view;

/// One frame of the zoom's own clock. The screen is live — rows appear as the
/// agent works — so unlike the pager it cannot sit on `events.next()`. Same
/// cadence as the inline host's ticker, so the spinner breathes at one rate.
const FRAME_MS: u64 = 40;

/// Which conversation the screen is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZoomTarget {
    /// A subagent instance: its whole record, and the composer routes to its
    /// inbox as the user.
    Agent(String),
    /// A room's log: the composer posts to it as the user, joining first.
    ///
    /// **The door is the background dialog's `f`** (D107). D105 built this view
    /// with no way in, deliberately: CC has no rooms at all — the concept does
    /// not exist anywhere in 2.1.88 — so there was no key of its to copy, and
    /// inventing a global binding for one surface is how a keymap rots. The
    /// dialog has a Rooms section and CC's `f to foreground` on it
    /// (`BackgroundTasksDialog.tsx:414`), so the room is foregrounded by the
    /// same key and the same verb as an agent.
    Room(String),
}

impl ZoomTarget {
    /// The name, without its sigil.
    pub fn name(&self) -> &str {
        match self {
            Self::Agent(name) | Self::Room(name) => name,
        }
    }

    /// The header's subject — the sigil is part of the identity.
    pub fn label(&self) -> String {
        match self {
            Self::Agent(name) => format!("@{name}"),
            Self::Room(name) => format!("#{name}"),
        }
    }

    /// Which conversation the accounting store is being pointed at.
    pub fn buffer(&self) -> BufferId {
        match self {
            Self::Agent(name) => BufferId::Dm(name.clone()),
            Self::Room(name) => BufferId::Channel(name.clone()),
        }
    }

    /// Where a submitted line goes. One grammar with the transcript's direct
    /// send (D103), because it is the same delivery — the zoom only spells the
    /// address differently.
    fn direct(&self) -> DirectTarget {
        match self {
            Self::Agent(name) => DirectTarget::Agent(name.clone()),
            Self::Room(name) => DirectTarget::Room(name.clone()),
        }
    }
}

/// What one key asks the loop to do. The meanings are pure; the loop owns the
/// terminal and the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Back to the transcript.
    Close,
    /// Point the same screen at somebody else — no leave, no re-enter.
    Switch(ZoomTarget),
}

/// The scroll position, measured from the bottom.
///
/// Zero is the live edge and the default: a view of a working agent that did
/// not follow its own tail would need a key to do the one thing it is for.
/// Scrolling up pins the offset, and the tail growing under a pinned offset
/// does not move the reader.
#[derive(Debug, Default)]
pub struct ZoomState {
    pub from_bottom: usize,
}

impl Chat {
    /// The agent whose conversation has the screen — read by the tree's stem
    /// and the pills' bold state (D104), both of which the zoom draws.
    ///
    /// A room zoom answers `None`: the pills and the tree list agents, and the
    /// row that would be lit is not on them.
    pub(crate) fn zoomed(&self) -> Option<&str> {
        match self.zoom.as_ref()? {
            ZoomTarget::Agent(name) => Some(name.as_str()),
            ZoomTarget::Room(_) => None,
        }
    }

    /// `enter` on a tree row, in either host (CC
    /// `useBackgroundTaskNavigation.ts:206-225`).
    ///
    /// The leader row leaves the view (`exitTeammateView`), the hide row
    /// collapses the tree, an instance row zooms it. Nothing at all with no row
    /// selected, which is what makes `enter` the composer's key while the tree
    /// is merely *open* — CC gates the whole handler on `selecting-agent` and
    /// so does this.
    pub(crate) fn tree_enter(&mut self) -> bool {
        let Some(selected) = self.tree_selection() else {
            return false;
        };
        let agents = self.tree_instances();
        self.dirty = true;
        if selected < 0 {
            // The leader row: leave the zoom if one is open, and leave
            // selection mode either way. The tree stays expanded, as CC's does.
            self.deselect_tree();
            self.close_zoom = self.zoom.is_some();
            return true;
        }
        match agents.get(selected as usize) {
            Some(status) => {
                let name = status.name.clone();
                self.open_zoom = Some(ZoomTarget::Agent(name));
            }
            // Past the end is the hide row: it closes the panel it closes.
            None => self.tree = None,
        }
        true
    }

    /// Leave selection mode without closing the tree — CC's escape in
    /// `selecting-agent` (`useBackgroundTaskNavigation.ts:168-176`).
    pub(crate) fn deselect_tree(&mut self) {
        if let Some(tree) = self.tree.as_mut() {
            tree.selected = None;
        }
        self.dirty = true;
    }

    /// `shift+tab` while zoomed: cycle the **viewed agent's** permission mode
    /// and leave the console's alone (CC `PromptInput.tsx:1410-1447`).
    ///
    /// Same ladder the console cycles through
    /// ([`Chat::cycle_permission_mode`]), read and written on the instance's own
    /// session. A room has no mode to cycle, and neither has a name the registry
    /// has forgotten.
    pub(crate) fn cycle_zoom_permission_mode(&mut self) -> bool {
        let Some(ZoomTarget::Agent(name)) = self.zoom.clone() else {
            return false;
        };
        let Some(mode) = self.session.agents.permission_mode_of(&name) else {
            return false;
        };
        let next = crate::tui::chat::next_permission_mode(mode, self.session.permission_mode);
        self.session.agents.set_permission_mode(&name, next);
        self.dirty = true;
        true
    }

    /// The permission mode the zoom's chrome reports: the viewed agent's, which
    /// is what CC swaps into the footer while a teammate has the screen
    /// (`PromptInput.tsx:342-351`).
    pub(crate) fn zoom_permission_mode(&self) -> Option<crate::permission::PermissionMode> {
        match self.zoom.as_ref()? {
            ZoomTarget::Agent(name) => self.session.agents.permission_mode_of(name),
            ZoomTarget::Room(_) => None,
        }
    }

    /// Who the composer is addressing, when it is not main: the zoom's subject,
    /// room included. The chrome tints itself with this.
    pub(crate) fn zoom_subject(&self) -> Option<String> {
        self.zoom.as_ref().map(|t| t.name().to_string())
    }

    /// Whether `esc` would stop a run rather than leave — what the hint row has
    /// to say out loud, because one key with two meanings has to declare which
    /// one it has right now (D39).
    pub(crate) fn zoom_stoppable(&self) -> bool {
        self.zoom_is_running()
    }

    /// The next instance on the roster, wrapping — what `shift+↑/↓` moves the
    /// zoom onto when the tree is not selecting.
    fn zoom_step(&self, delta: isize) -> Option<ZoomTarget> {
        let agents = self.tree_instances();
        if agents.is_empty() {
            return None;
        }
        let here = agents
            .iter()
            .position(|s| Some(s.name.as_str()) == self.zoomed())?;
        let len = agents.len() as isize;
        let next = (here as isize + delta).rem_euclid(len) as usize;
        (next != here).then(|| ZoomTarget::Agent(agents[next].name.clone()))
    }

    /// Whether the viewed agent is running right now.
    pub(crate) fn zoom_is_running(&self) -> bool {
        let Some(name) = self.zoomed() else {
            return false;
        };
        self.tree_instances()
            .iter()
            .any(|s| s.name == name && s.state == AgentState::Running)
    }

    /// Whether the view has to close itself: the instance is gone (CC
    /// `useTeammateViewAutoExit.ts:35`) or its run failed
    /// (`:52`). A *finished* agent's view stays open, which is CC's ruling in
    /// as many words — "Users stay viewing completed teammates so they can
    /// review the full transcript" (`:8-9`) — and bingo's design ruling too.
    ///
    /// bingo has no `failed` instance state: a run that dies leaves the
    /// instance idle and writes the alert line into `@main` (D98). So the
    /// condition that survives the port is the one that is expressible — the
    /// name has left the registry — and a failure is read where it is written.
    fn zoom_is_gone(&self) -> bool {
        match self.zoom.as_ref() {
            Some(ZoomTarget::Agent(name)) => !self.tree_instances().iter().any(|s| &s.name == name),
            Some(ZoomTarget::Room(name)) => !self
                .session
                .channels
                .list()
                .iter()
                .any(|status| &status.name == name),
            None => false,
        }
    }

    /// Point the screen at a conversation: the accounting follows the reader,
    /// so entering one **reads** it and nothing in it is unread while it is up
    /// ([`crate::tui::buffer::Buffers::set_active`], which also clears the
    /// mention flag).
    pub(crate) fn enter_zoom(&mut self, target: ZoomTarget) {
        self.refresh_conversations();
        self.buffers.set_active(target.buffer());
        self.zoom = Some(target);
        self.close_zoom = false;
        self.dirty = true;
    }

    /// Give the screen back. The accounting returns to whatever it was pointed
    /// at before, and the tree's cursor is dropped — it belongs to the gesture
    /// that opened the view, not to the transcript underneath.
    pub(crate) fn leave_zoom(&mut self, home: BufferId) {
        self.zoom = None;
        self.close_zoom = false;
        self.buffers.set_active(home);
        self.deselect_tree();
        self.dirty = true;
    }

    /// Submit the draft to the conversation on screen.
    ///
    /// One delivery path with the transcript's `@name`/`#room` send (D103):
    /// [`Chat::direct_send`] puts it in the inbox (or the room's log) under the
    /// user's own name, which is where the domain's own rules take over —
    /// queue while the run is busy, drain at the next round, resume an instance
    /// that had stopped, announce a join before speaking in a room.
    ///
    /// **The draft is plain text.** A `/` line is not a command and a `!` line
    /// is not a shell: neither ever reaches the console's parser, because the
    /// console is not what this composer is addressing. CC does the same by
    /// construction — the teammate route returns before any slash handling
    /// (`PromptInput.tsx:1086-1097`, `REPL.tsx:3548-3578`) — and here it is by
    /// construction too, since nothing but this function reads the draft.
    ///
    /// **The echo is the receipt.** A message the user just sent is in the
    /// instance's inbox, so the very next frame draws it as a queued bubble;
    /// the transcript needs a `Sent to @scout` line because it has nowhere else
    /// to show one, and this view is the somewhere else.
    pub(crate) fn submit_to_zoom(&mut self) {
        let Some(target) = self.zoom.clone() else {
            return;
        };
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.undo.clear();
        self.last_edit = None;
        if text.trim().is_empty() {
            self.set_input(text);
            return;
        }
        let body = self.expand_pastes(&text);
        let body = self.expand_image_paths(&body);
        // The whole line goes into history, exactly as the transcript's own
        // submit records it: ↑ brings back what was typed.
        self.record_history(&text);
        // A refusal is news and takes the warning tier the stop takes; the
        // *receipt* the transcript prints is not needed here, because what it
        // would announce is drawn on this screen the moment the frame redraws.
        if let crate::tui::buffer::Delivery::Rejected(why) =
            self.deliver_direct(&target.direct(), body)
        {
            self.push_warning(why);
        }
        self.update_slash_suggestions();
        self.dirty = true;
    }

    /// Every key the zoom does not claim edits the draft.
    ///
    /// It is the console's own composer — the readline keys, the kill ring, the
    /// history, the paste folding — reached through [`Chat::on_key`] so there is
    /// one editor and not two. What is held back is the set of keys that open a
    /// surface this screen cannot show: they would arm a modal the host would
    /// spring the moment the zoom closed, which is worse than a key that does
    /// nothing.
    pub(crate) fn zoom_edit_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if modifiers.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('o' | 'O' | 'b' | 't' | 'g' | 'r' | 'e'))
        {
            return false;
        }
        self.on_key(code, modifiers)
    }

    /// The zoom's body: the conversation, in the vocabulary every other view
    /// draws it in.
    ///
    /// An agent's is its **whole record** ([`zoom_posts`], the user's ruling):
    /// the task that created it, main's instructions, room relays, reminders,
    /// its process and its answers. A room's is the room's log, membership
    /// lines included ([`channel_posts`]) — reading a room is free, so this is
    /// the same log every member sees.
    pub(crate) fn zoom_posts_of(&self, target: &ZoomTarget) -> Vec<Post> {
        match target {
            ZoomTarget::Agent(name) => {
                let Some((history, stamps, live, in_flight, _state)) =
                    self.session.agents.view_of(name)
                else {
                    return Vec::new();
                };
                let pending = self.session.agents.pending_of(name);
                zoom_posts(&history, &stamps, &in_flight, &live, &pending, name)
            }
            ZoomTarget::Room(name) => channel_posts(&self.session.channels.log_of(name), USER_NAME),
        }
    }

    /// The two header rows: who is being viewed, and what they were asked for.
    ///
    /// CC's copy exactly (`components/TeammateViewHeader.tsx:31-62`): `Viewing `
    /// plain, `@name` bold in the identity colour, ` · esc to return` dim, and
    /// the task prompt dim on its own line. The blank row under it is CC's
    /// `marginBottom={1}` (`:70`).
    pub(crate) fn zoom_header(&self, target: &ZoomTarget, width: usize) -> Vec<Row> {
        let theme = &self.theme;
        let palette = crate::tui::avatar::Palette::new(theme);
        let gutter = crate::tui::avatar::Gutter::new(false, &palette, &self.faces_pinned);
        let identity = palette.avatars[gutter.index_for(target.name()) % palette.avatars.len()];
        let mut line = Line::styled("Viewing ", SegStyle::fg(theme.text));
        line.push_styled(target.label(), SegStyle::fg(identity).bold());
        line.push_styled(
            " · esc to return".to_string(),
            SegStyle::fg(theme.text_secondary),
        );
        let subtitle = match target {
            ZoomTarget::Agent(name) => self
                .tree_instances()
                .iter()
                .find(|s| &s.name == name)
                .map(|s| {
                    if s.prompt.trim().is_empty() {
                        s.description.clone()
                    } else {
                        s.prompt.clone()
                    }
                })
                .unwrap_or_default(),
            // A room was not dispatched with a task, so the line under its name
            // says the thing a room's identity actually is: who is in it.
            ZoomTarget::Room(name) => self
                .session
                .channels
                .list()
                .iter()
                .find(|status| &status.name == name)
                .map(|status| status.members.join(", "))
                .unwrap_or_default(),
        };
        vec![
            Row::new(one_line_at(line, width)),
            Row::new(Line::styled(
                one_line(subtitle.lines().next().unwrap_or_default(), width),
                SegStyle::fg(theme.text_secondary),
            )),
            Row::new(Line::empty()),
        ]
    }

    /// The whole screen above the composer: the header and the conversation,
    /// through the one post renderer and the one avatar gutter (D97/D99), so a
    /// message looks like itself wherever it is read.
    pub(crate) fn zoom_rows(&self, target: &ZoomTarget, width: usize) -> Vec<Row> {
        let palette = crate::tui::avatar::Palette::new(&self.theme);
        let gutter = self.conversation_gutter(&palette);
        let posts = self.zoom_posts_of(target);
        let runs = sender_runs(&posts);
        let mut rows = self.zoom_header(target, width);
        for (post, lead) in posts.iter().zip(runs) {
            rows.extend(self.zoom_post_rows(post, target.name(), width, &gutter, lead));
        }
        rows
    }
}

/// A header line cut to the canvas. The pieces are already styled, so this
/// cannot go through [`one_line`] — it trims the composed line instead.
fn one_line_at(line: Line, width: usize) -> Line {
    if crate::tui::line::text_width(&line.plain_text()) <= width {
        return line;
    }
    let mut out = Line::empty();
    let mut budget = width;
    for seg in line.segs {
        if budget == 0 {
            break;
        }
        let cut = one_line(&seg.text, budget);
        budget = budget.saturating_sub(crate::tui::line::text_width(&cut));
        out.push_styled(cut, seg.style);
    }
    out
}

/// Open the zoom on the alternate screen. Mirrors
/// [`crate::tui::transcript::run_transcript_modal`], including the guarded
/// enter and the D77 panic-hook claim.
pub async fn run_zoom_modal(
    chat: &mut Chat,
    events: &mut EventStream,
    target: ZoomTarget,
    already_alt: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let _claim = (!already_alt).then(crate::tui::AltScreenClaim::enter);
    if !already_alt && let Err(e) = execute!(stdout(), EnterAlternateScreen, EnableMouseCapture) {
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        return Err(e.into());
    }
    let result = modal_loop(chat, events, target).await;
    if !already_alt {
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
    result
}

/// Rows the conversation gets: everything but the composer and the status
/// layer under it.
fn viewport_of(height: usize, chrome: usize) -> usize {
    height.saturating_sub(chrome).max(1)
}

async fn modal_loop(
    chat: &mut Chat,
    events: &mut EventStream,
    target: ZoomTarget,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    // Where the accounting was pointed before the zoom took the screen. The
    // home conversation is not assumed: the pointer is restored to whatever it
    // was, which is what makes entering and leaving symmetric.
    let home = chat.buffers.active().clone();
    chat.enter_zoom(target);
    let mut state = ZoomState::default();
    // Portraits: the gutter draws them on this screen too, and the alternate
    // screen's image store is the terminal's own — a resize can purge it, so
    // the bookkeeping starts empty and all eight go once, as the perspective
    // page sends them.
    let mut transmits = crate::tui::gfx::Transmits::default();
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(FRAME_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // The view lives as long as it has a subject: `None` is the way out every
    // `Action::Close` takes, and `zoom_is_gone` is the auto-return.
    while let Some(target) = chat.zoom.clone() {
        // `ctrl+c` is not rebound here — it is the app's interrupt-then-quit
        // contract and rebinding a quit key inside one view is how a quit key
        // becomes unpredictable. What the view owes it is to get out of the way
        // at once rather than let the host quit on the way back.
        if chat.zoom_is_gone() || chat.exit {
            break;
        }
        let size = terminal.size()?;
        let width = size.width as usize;
        if let Some(cap) = chat.image_cap {
            let faces: Vec<usize> = (0..crate::tui::avatar::COUNT).collect();
            let bytes = crate::tui::avatar::transmits(&faces, &cap, &mut transmits);
            crate::tui::term::write_transmits(terminal.backend_mut(), &bytes)?;
        }
        let chrome = crate::tui::chrome::zoom_chrome(chat, width);
        let chrome_rows =
            crate::tui::statics::layout(vec![crate::tui::statics::Block::live(chrome)]).rows;
        let viewport = viewport_of(size.height as usize, chrome_rows.len());
        let rows = chat.zoom_rows(&target, width);
        state.from_bottom = state.from_bottom.min(rows.len().saturating_sub(1));
        let end = rows.len().saturating_sub(state.from_bottom);
        let start = end.saturating_sub(viewport);
        let visible: Vec<Row> = rows[start..end].to_vec();
        let theme = chat.theme.clone();
        terminal.draw(|frame| {
            let area = frame.area();
            let buf = frame.buffer_mut();
            let body = ratatui::layout::Rect::new(
                area.x,
                area.y,
                area.width,
                area.height.saturating_sub(chrome_rows.len() as u16),
            );
            view::render_rows(&visible, theme.text, buf, body);
            if (chrome_rows.len() as u16) <= area.height {
                let y = area.y + area.height - chrome_rows.len() as u16;
                let chrome_area =
                    ratatui::layout::Rect::new(area.x, y, area.width, chrome_rows.len() as u16);
                view::render_rows(&chrome_rows, theme.text, buf, chrome_area);
            }
        })?;

        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    match on_key(chat, &mut state, key.code, key.modifiers, viewport) {
                        Action::Close => break,
                        Action::Switch(next) => {
                            chat.enter_zoom(next);
                            state.from_bottom = 0;
                        }
                        Action::None => {}
                    }
                }
                Some(Ok(Event::Paste(text))) => {
                    chat.on_paste(&text);
                }
                Some(Ok(Event::Mouse(mouse))) => match mouse.kind {
                    MouseEventKind::ScrollUp => state.from_bottom = state.from_bottom.saturating_add(3),
                    MouseEventKind::ScrollDown => state.from_bottom = state.from_bottom.saturating_sub(3),
                    _ => {}
                },
                Some(Ok(Event::Resize(_, _))) => {
                    transmits.reset();
                    terminal.clear()?;
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
            _ = ticker.tick() => {
                // The zoom's only write to the host's clock. `Chat::tick` is
                // deliberately *not* called: it drains, debounces and polls on
                // behalf of a transcript that is not on screen, and the whole
                // guarantee of this view is that the transcript is untouched
                // while it is open. What the counter buys is the spinner.
                chat.tick = chat.tick.wrapping_add(1);
            }
        }
    }
    chat.leave_zoom(home);
    Ok(())
}

/// One key inside the zoom. Pure: the loop owns the terminal, this owns the
/// meaning.
///
/// The order is CC's ladder. `esc` peels — the tree's cursor, then the running
/// turn, then the view — because CC's single `viewSelectionMode` makes exactly
/// those three states and answers each in turn
/// (`useBackgroundTaskNavigation.ts:151-176`), and because one press means one
/// level is D80's rule here as everywhere.
pub fn on_key(
    chat: &mut Chat,
    state: &mut ZoomState,
    code: KeyCode,
    modifiers: KeyModifiers,
    viewport: usize,
) -> Action {
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    match code {
        KeyCode::Esc => {
            // A cursor in the tree is the topmost thing Esc can take.
            if chat.tree_selection().is_some() {
                chat.deselect_tree();
                return Action::None;
            }
            // Running: abort the turn and stay. The instance keeps its history
            // and a message resumes it — which is exactly what CC's
            // `currentWorkAbortController` does and not what its
            // `abortController` does.
            if chat.zoom_is_running() {
                if let Some(name) = chat.zoomed().map(str::to_string) {
                    chat.stop_agent(&name);
                }
                return Action::None;
            }
            Action::Close
        }
        // The tree's cursor moves under the zoom exactly as it moves under the
        // transcript, and `enter` then means what it means there.
        KeyCode::Up | KeyCode::Down if shift && !chat.tree_instances().is_empty() => {
            if chat.tree_selection().is_some() || chat.tree.is_some() {
                chat.agent_tree_key(code, modifiers);
                return Action::None;
            }
            // No tree open: the chord walks the roster instead of opening a
            // panel nobody asked for. CC steps its tree because its tree is on
            // screen; here the screen *is* the agent, so the step moves it.
            match chat.zoom_step(if code == KeyCode::Down { 1 } else { -1 }) {
                Some(next) => Action::Switch(next),
                None => Action::None,
            }
        }
        KeyCode::BackTab => {
            chat.cycle_zoom_permission_mode();
            Action::None
        }
        KeyCode::PageUp => {
            state.from_bottom = state.from_bottom.saturating_add(viewport.max(1));
            Action::None
        }
        KeyCode::PageDown => {
            state.from_bottom = state.from_bottom.saturating_sub(viewport.max(1));
            Action::None
        }
        KeyCode::Enter if !modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
            // A selected tree row owns Enter — CC gates its whole handler on
            // selection mode, so with nothing selected Enter is the composer's.
            if chat.tree_selection().is_some() {
                if chat.tree_enter() {
                    if let Some(next) = chat.open_zoom.take() {
                        return Action::Switch(next);
                    }
                    if std::mem::take(&mut chat.close_zoom) {
                        return Action::Close;
                    }
                }
                return Action::None;
            }
            chat.submit_to_zoom();
            state.from_bottom = 0;
            Action::None
        }
        _ => {
            chat.zoom_edit_key(code, modifiers);
            Action::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentKind, AgentProgress, LiveBlock};
    use crate::api::types::{ContentBlock, Message as ApiMessage, Role as ApiRole};
    use crate::channels::{ChannelMode, MAIN_NAME};
    use crate::permission::PermissionMode;
    use crate::tui::test_util::chat_at;

    fn test_chat() -> Chat {
        chat_at(100, 40)
    }

    /// An instance the registry has just been told about — which is a
    /// *running* one: `insert` is the spawn path and a spawned agent is working.
    fn seed(chat: &Chat, name: &str) {
        chat.session.agents.insert(
            name,
            AgentKind::Hire,
            None,
            "test instance".to_string(),
            chat.session.clone(),
        );
    }

    /// The same instance between runs, which is what most of these are about.
    fn seed_idle(chat: &Chat, name: &str) {
        seed(chat, name);
        chat.session.agents.mark_idle(name);
    }

    fn assistant(text: &str) -> ApiMessage {
        ApiMessage {
            role: ApiRole::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    fn seed_with_history(chat: &Chat, name: &str, history: Vec<ApiMessage>) {
        seed(chat, name);
        chat.session.agents.finish(name, history, 0);
    }

    fn seed_room(chat: &Chat, name: &str, members: &[&str]) {
        chat.session
            .channels
            .create(
                name,
                members.iter().map(|m| m.to_string()).collect(),
                ChannelMode::Free,
            )
            .expect("room created");
    }

    fn rows(chat: &Chat, target: &ZoomTarget) -> Vec<String> {
        chat.zoom_rows(target, 100)
            .iter()
            .map(|row| row.line.plain_text().trim_end().to_string())
            .collect()
    }

    /// Drive one key the way the loop drives it.
    fn key(chat: &mut Chat, code: KeyCode, modifiers: KeyModifiers) -> Action {
        let mut state = ZoomState::default();
        on_key(chat, &mut state, code, modifiers, 20)
    }

    // -- the view ----------------------------------------------------------

    /// CC's header, word for word: `Viewing ` plain, the name bold in its own
    /// colour, ` · esc to return` dim, then the task the instance was
    /// dispatched with on its own dim line, then a blank
    /// (`TeammateViewHeader.tsx:31-70`).
    #[test]
    fn the_header_is_ccs_line_and_the_task_under_it() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        chat.session
            .agents
            .set_prompt("scout", "Find the rendering seam".into());
        let target = ZoomTarget::Agent("scout".into());
        let head = chat.zoom_header(&target, 100);
        let text: Vec<String> = head
            .iter()
            .map(|r| r.line.plain_text().trim_end().to_string())
            .collect();
        assert_eq!(
            text,
            vec![
                "Viewing @scout · esc to return".to_string(),
                "Find the rendering seam".to_string(),
                String::new(),
            ]
        );
        // The name is the one bold segment, and it is not the console's colour.
        let name_seg = head[0]
            .line
            .segs
            .iter()
            .find(|seg| seg.text.contains("@scout"))
            .expect("the name is its own segment");
        assert!(name_seg.style.bold, "CC bolds the name it colours");
        chat.zoom = Some(target);
        assert_eq!(chat.zoomed(), Some("scout"));
    }

    /// The body is the agent's whole record — the task it was given, main's
    /// instructions, its own work and its answers — which is the user's ruling
    /// and the difference from every view bingo had before.
    #[test]
    fn the_body_is_the_whole_record_and_not_one_lane() {
        let chat = test_chat();
        seed_with_history(
            &chat,
            "scout",
            vec![
                ApiMessage::user_text("audit the lexer"),
                ApiMessage {
                    role: ApiRole::Assistant,
                    content: vec![
                        ContentBlock::ToolUse {
                            id: "t1".into(),
                            name: "Read".into(),
                            input: serde_json::json!({"file_path": "a.rs"}),
                        },
                        ContentBlock::Text {
                            text: "one leak".into(),
                        },
                    ],
                },
                ApiMessage::user_text(format!(
                    "{}\nand the parser?",
                    crate::tool::agent::DM_FROM_USER_MARKER
                )),
                assistant("clean"),
            ],
        );
        let body = rows(&chat, &ZoomTarget::Agent("scout".into())).join("\n");
        for expected in [
            "audit the lexer",
            "⏺ Read 1 file",
            "one leak",
            "and the parser?",
            "clean",
        ] {
            assert!(body.contains(expected), "missing {expected:?}: {body}");
        }
    }

    /// The tail is live: a stream block the registry gained since the last
    /// frame is on the next one, without anything being appended to a store.
    #[test]
    fn the_tail_follows_the_run() {
        let chat = test_chat();
        seed_with_history(&chat, "scout", vec![ApiMessage::user_text("go")]);
        let target = ZoomTarget::Agent("scout".into());
        let before = rows(&chat, &target).join("\n");
        assert!(!before.contains("halfway"));

        chat.session.agents.set_live(
            "scout",
            Some(std::sync::Arc::new(std::sync::Mutex::new(vec![
                LiveBlock::Text("halfway there".into()),
            ]))),
            None,
        );
        let after = rows(&chat, &target).join("\n");
        assert!(
            after.contains("halfway there"),
            "the stream reaches the screen: {after}"
        );
    }

    /// A room's log, membership lines included. Reading one is free, so this is
    /// the same log every member sees.
    #[test]
    fn a_room_zoom_is_the_rooms_own_log() {
        let chat = test_chat();
        seed_room(&chat, "build", &["scout"]);
        chat.session
            .channels
            .post("scout", "build", "tests are green")
            .expect("posted");
        let body = rows(&chat, &ZoomTarget::Room("build".into())).join("\n");
        assert!(body.contains("Viewing #build · esc to return"), "{body}");
        assert!(body.contains("tests are green"), "{body}");
    }

    // -- the composer ------------------------------------------------------

    /// The central promise: the draft reaches the agent's inbox as the user,
    /// through the one delivery path the transcript's `@name` line uses, and
    /// main's history is untouched.
    #[tokio::test]
    async fn typing_in_a_zoom_reaches_the_agent_as_the_user() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        let before = chat.messages.len();

        chat.set_input("have a look at the parser");
        key(&mut chat, KeyCode::Enter, KeyModifiers::NONE);

        assert!(!chat.busy, "the console did not take a turn");
        assert!(chat.queued.is_empty());
        assert_eq!(chat.messages.len(), before, "and wrote nothing in main");
        assert!(chat.input.is_empty(), "the composer cleared");
        assert!(
            chat.slash_info_lines.is_empty(),
            "no receipt: the message itself is on screen"
        );

        let items = chat.session.agents.take_running("scout", 0);
        let (prompt, _) = crate::tool::agent::absorb_inbox(&chat.session.channels, "scout", &items);
        assert_eq!(
            prompt,
            format!(
                "{}\nhave a look at the parser",
                crate::tool::agent::DM_FROM_USER_MARKER
            ),
        );
    }

    /// The echo is immediate: the very next frame draws what was just sent, as
    /// a queued bubble under the user's name. That is why the zoom needs no
    /// `Sent to @scout` receipt — it *is* the receipt.
    #[tokio::test]
    async fn the_send_echoes_on_the_next_frame() {
        let mut chat = test_chat();
        seed_with_history(&chat, "scout", vec![ApiMessage::user_text("start")]);
        let target = ZoomTarget::Agent("scout".into());
        chat.enter_zoom(target.clone());
        chat.set_input("and the parser?");
        key(&mut chat, KeyCode::Enter, KeyModifiers::NONE);
        let body = rows(&chat, &target).join("\n");
        assert!(
            body.contains("and the parser?"),
            "the message is on screen the moment it is sent: {body}"
        );
    }

    /// **A message to a stopped instance resumes it** (CC subagent semantics,
    /// `SendMessageTool.ts:808-866`): the registry kept its session and
    /// history, so the delivery flips it off `Stopped` and the flush respawns
    /// the run. D105 found this unimplemented and pinned the refusal here; the
    /// review fixup implemented it, and this is the same pin facing the other
    /// way — nothing on the warning tier, because something *did* happen.
    #[tokio::test]
    async fn a_message_to_a_stopped_agent_resumes_it() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.session.agents.stop("scout").expect("stopped");
        assert_eq!(
            chat.tree_instances()[0].state,
            AgentState::Stopped,
            "the instance is on the roster and stopped"
        );

        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        chat.set_input("carry on");
        key(&mut chat, KeyCode::Enter, KeyModifiers::NONE);

        assert_ne!(
            chat.tree_instances()[0].state,
            AgentState::Stopped,
            "the delivery woke the instance from its kept history"
        );
        assert!(
            chat.visible_warning().is_none(),
            "nothing was refused: {:?}",
            chat.visible_warning()
        );
    }

    /// Slash and bash lines are plain text here. CC routes to the teammate
    /// before any slash handling (`PromptInput.tsx:1086-1097`, `REPL.tsx:3548-3578`)
    /// and never re-prepends the `!`; bingo does it by construction, because
    /// nothing but the zoom's own submit reads this draft.
    #[tokio::test]
    async fn a_slash_line_in_a_zoom_is_a_message_and_not_a_command() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        chat.set_input("/help");
        key(&mut chat, KeyCode::Enter, KeyModifiers::NONE);

        assert!(
            chat.slash_lines.is_empty() && chat.slash_info_lines.is_empty(),
            "the console's parser never saw it"
        );
        let items = chat.session.agents.take_running("scout", 0);
        let (prompt, _) = crate::tool::agent::absorb_inbox(&chat.session.channels, "scout", &items);
        assert!(prompt.ends_with("/help"), "it went as typed: {prompt}");
    }

    /// Posting to a room the user is only reading joins first, through the one
    /// join path, so every member sees the arrival the joiner does.
    #[tokio::test]
    async fn a_room_zoom_joins_before_it_speaks() {
        let mut chat = test_chat();
        seed_room(&chat, "build", &["scout"]);
        assert!(!chat.session.channels.is_member("build", USER_NAME));

        chat.enter_zoom(ZoomTarget::Room("build".into()));
        chat.set_input("nice");
        key(&mut chat, KeyCode::Enter, KeyModifiers::NONE);

        assert!(chat.session.channels.is_member("build", USER_NAME));
        let log = chat.session.channels.log_of("build");
        assert!(
            log.iter()
                .any(|m| m.kind == crate::channels::MessageKind::Membership && m.from == USER_NAME),
            "the join is announced in the room's own log: {log:?}"
        );
        assert!(log.iter().any(|m| m.text == "nice" && m.from == USER_NAME));
    }

    // -- the keys ----------------------------------------------------------

    /// `esc` has two meanings and takes them in CC's order: a running agent's
    /// turn is aborted and the view stays; with nothing running it returns
    /// (`useBackgroundTaskNavigation.ts:151-165`).
    #[test]
    fn esc_stops_the_run_first_and_returns_second() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.session.agents.set_progress_snapshot(
            "scout",
            AgentProgress {
                started_at: Some(std::time::Instant::now()),
                ..AgentProgress::default()
            },
        );
        chat.session
            .agents
            .deliver("scout", MAIN_NAME, "go", Vec::new(), None)
            .ok();
        crate::tool::agent::flush_agent_inbox(&chat.session, &chat.session.watch);
        assert_eq!(chat.tree_instances()[0].state, AgentState::Running);

        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        assert_eq!(
            key(&mut chat, KeyCode::Esc, KeyModifiers::NONE),
            Action::None,
            "the first press stops the run and stays"
        );
        assert_eq!(chat.tree_instances()[0].state, AgentState::Stopped);
        assert!(
            !chat.tree_instances().is_empty(),
            "stopping is not deleting: the instance keeps its history and can be resumed"
        );
        assert_eq!(
            key(&mut chat, KeyCode::Esc, KeyModifiers::NONE),
            Action::Close,
            "the second press leaves"
        );
    }

    /// And a cursor in the tree is peeled before either of them — CC answers
    /// `selecting-agent` with its own escape branch (`:168-176`), and one press
    /// is one level (D80).
    #[test]
    fn esc_peels_the_tree_cursor_before_the_view() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        chat.open_agent_tree();
        key(&mut chat, KeyCode::Down, KeyModifiers::SHIFT);
        assert!(chat.tree_selection().is_some());

        assert_eq!(
            key(&mut chat, KeyCode::Esc, KeyModifiers::NONE),
            Action::None
        );
        assert!(chat.tree_selection().is_none(), "the cursor went first");
        assert!(chat.tree.is_some(), "and the tree stayed, as CC's does");
        assert_eq!(
            key(&mut chat, KeyCode::Esc, KeyModifiers::NONE),
            Action::Close
        );
    }

    /// `shift+tab` cycles the **viewed agent's** permission mode and leaves the
    /// console's alone — CC's whole point in `handleCycleMode`
    /// (`PromptInput.tsx:1410-1447`).
    #[test]
    fn shift_tab_cycles_the_viewed_agents_mode_and_not_mains() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        seed_idle(&chat, "writer");
        let before_main = chat.permission_mode;
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));

        key(&mut chat, KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(
            chat.session.agents.permission_mode_of("scout"),
            Some(PermissionMode::AcceptEdits),
            "one step along the console's own ladder"
        );
        assert_eq!(
            chat.zoom_permission_mode(),
            Some(PermissionMode::AcceptEdits)
        );
        key(&mut chat, KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(
            chat.session.agents.permission_mode_of("scout"),
            Some(PermissionMode::Plan)
        );

        assert_eq!(
            chat.permission_mode, before_main,
            "main's mode is untouched"
        );
        assert_eq!(
            chat.session.agents.permission_mode_of("writer"),
            Some(PermissionMode::Default),
            "and so is every other instance's"
        );
    }

    /// With no tree on screen, `shift+↑/↓` walks the roster and the view
    /// follows. CC steps its *tree* there because its tree is visible; here the
    /// screen is the agent, so the step moves the screen.
    #[test]
    fn shift_arrows_walk_the_roster_when_no_tree_is_open() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        seed_idle(&chat, "writer");
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));

        assert_eq!(
            key(&mut chat, KeyCode::Down, KeyModifiers::SHIFT),
            Action::Switch(ZoomTarget::Agent("writer".into()))
        );
        chat.enter_zoom(ZoomTarget::Agent("writer".into()));
        assert_eq!(
            key(&mut chat, KeyCode::Down, KeyModifiers::SHIFT),
            Action::Switch(ZoomTarget::Agent("scout".into())),
            "and it wraps"
        );
    }

    /// With the tree open it is the tree's cursor that moves, and `enter` then
    /// means what CC says it means: the leader row returns, the hide row
    /// collapses the panel, an instance row switches the view
    /// (`useBackgroundTaskNavigation.ts:206-225`).
    #[test]
    fn enter_on_a_tree_row_returns_collapses_or_switches() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        seed_idle(&chat, "writer");
        let step = |chat: &mut Chat, n: usize| {
            for _ in 0..n {
                key(chat, KeyCode::Down, KeyModifiers::SHIFT);
            }
        };

        // The ring is CC's: instances, the hide row, then `@main` at the wrap
        // (`useBackgroundTaskNavigation.ts:26-58`). Four steps past two
        // instances lands on the leader.
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        chat.open_agent_tree();
        step(&mut chat, 4);
        assert_eq!(
            key(&mut chat, KeyCode::Enter, KeyModifiers::NONE),
            Action::Close,
            "the leader row is the way back to the transcript"
        );

        // An instance row: the same screen, somebody else on it.
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        chat.open_agent_tree();
        step(&mut chat, 2);
        assert_eq!(
            key(&mut chat, KeyCode::Enter, KeyModifiers::NONE),
            Action::Switch(ZoomTarget::Agent("writer".into()))
        );

        // The hide row: the panel closes and the view stays.
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        chat.open_agent_tree();
        step(&mut chat, 3);
        assert_eq!(
            key(&mut chat, KeyCode::Enter, KeyModifiers::NONE),
            Action::None
        );
        assert!(chat.tree.is_none(), "the hide row hides");
        assert!(
            chat.zoom.is_some(),
            "and hiding a panel is not leaving a view"
        );
    }

    /// With nothing selected, `enter` is the composer's — which is what makes a
    /// status panel safe to leave open while typing (CC gates its whole handler
    /// on selection mode).
    #[tokio::test]
    async fn enter_is_the_composers_while_the_tree_is_merely_open() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        chat.open_agent_tree();
        assert!(chat.tree_selection().is_none());

        chat.set_input("still talking to you");
        key(&mut chat, KeyCode::Enter, KeyModifiers::NONE);
        assert!(chat.input.is_empty());
        assert!(chat.tree.is_some(), "and the panel did not move");
        assert_eq!(chat.session.agents.pending_of("scout").len(), 1);
    }

    /// The keys that would arm a surface this screen cannot show are held back
    /// rather than passed to the composer: a modal the host would spring on the
    /// way out is worse than a key that does nothing.
    #[test]
    fn the_zoom_holds_back_the_keys_that_open_other_surfaces() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        for c in ['o', 'b', 't', 'g', 'r', 'e'] {
            key(&mut chat, KeyCode::Char(c), KeyModifiers::CONTROL);
        }
        assert!(!chat.open_transcript, "ctrl+o did not arm the pager");
        assert!(chat.dialog.is_none(), "ctrl+b did not arm the dialog");
        assert!(!chat.open_editor, "ctrl+g did not arm the editor");
        assert!(chat.search.is_none(), "ctrl+r did not arm the search");
        assert!(chat.input.is_empty(), "and none of them typed a character");
    }

    /// `ctrl+c` keeps the app's meaning — interrupt, then quit on a second
    /// press — and the view gets out of the way rather than letting the host
    /// quit on the way back. The window it arms is announced where every other
    /// transient notice is announced.
    #[test]
    fn ctrl_c_keeps_its_own_contract_and_the_view_stands_aside() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        key(&mut chat, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(!chat.exit, "one press arms the window");
        assert!(
            chat.notice.is_some_and(|n| n.contains("ctrl-c")),
            "and says so: {:?}",
            chat.notice
        );
        let text = crate::tui::statics::layout(vec![crate::tui::statics::Block::live(
            crate::tui::chrome::zoom_chrome(&chat, 100),
        )])
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
        assert!(text.contains("ctrl-c"), "on this screen too: {text}");

        key(&mut chat, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(chat.exit, "the second press quits");
    }

    /// An ordinary key still edits the draft, through the console's own
    /// composer — one editor, not two.
    #[test]
    fn every_other_key_edits_the_draft() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        for c in "hi".chars() {
            key(&mut chat, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(chat.input, "hi");
        key(&mut chat, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(chat.input, "h");
    }

    // -- lifecycle ---------------------------------------------------------

    /// Entering reads the conversation: the accounting points at it and its
    /// unread and mention go. Leaving points the accounting back at whatever it
    /// was on, rather than at an assumed home.
    #[test]
    fn entering_reads_the_conversation_and_leaving_gives_it_back() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        // Materialize the buffer first: a conversation seen for the first time
        // starts read, so what follows has to be new.
        chat.refresh_conversations();
        chat.session.agents.finish(
            "scout",
            vec![
                ApiMessage::user_text(format!(
                    "{}\nhave a look",
                    crate::tool::agent::DM_FROM_USER_MARKER
                )),
                assistant("@user look at this"),
            ],
            0,
        );
        chat.refresh_conversations();
        let id = BufferId::Dm("scout".into());
        assert!(chat.buffers.get(&id).is_some_and(|b| b.unread() > 0));
        assert!(chat.buffers.get(&id).is_some_and(|b| b.mention()));

        let home = chat.buffers.active().clone();
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        assert_eq!(
            chat.buffers.active(),
            &id,
            "the reader points at what it reads"
        );
        assert_eq!(chat.buffers.get(&id).map(|b| b.unread()), Some(0));
        assert!(!chat.buffers.get(&id).is_some_and(|b| b.mention()));

        chat.leave_zoom(home.clone());
        assert_eq!(chat.buffers.active(), &home);
        assert!(chat.zoom.is_none());
    }

    /// The view closes itself when the instance is no longer there — CC exits
    /// on an evicted task (`useTeammateViewAutoExit.ts:35`). A *finished* one
    /// stays open, which is CC's own ruling (`:8-9`) and the design's.
    #[test]
    fn the_view_closes_when_the_agent_is_gone_and_stays_when_it_is_merely_done() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        chat.session.agents.stop("scout").expect("stopped");
        assert!(!chat.zoom_is_gone(), "a stopped agent is still an agent");
        chat.session.agents.mark_idle("scout");
        assert!(!chat.zoom_is_gone(), "and so is a finished one");

        chat.session.agents.remove("scout").expect("removed");
        assert!(chat.zoom_is_gone(), "a name off the roster has no view");
    }

    /// A room answers the same question the same way. The registry has no
    /// removal today, so the reachable half is a name that is not a room — the
    /// condition is stated for the whole target rather than for one arm of it,
    /// because that is what makes the guard correct when a removal arrives.
    #[test]
    fn a_room_zoom_closes_when_the_room_is_not_there() {
        let mut chat = test_chat();
        seed_room(&chat, "build", &["scout"]);
        chat.enter_zoom(ZoomTarget::Room("build".into()));
        assert!(!chat.zoom_is_gone());
        chat.enter_zoom(ZoomTarget::Room("ghost".into()));
        assert!(chat.zoom_is_gone());
    }

    // -- the round trip ----------------------------------------------------

    /// **The property this batch is riskiest against.** A zoom is not allowed
    /// to disturb the transcript underneath it: the inline host prints each
    /// settled row into scrollback exactly once, so a row printed twice or lost
    /// cannot be undone.
    ///
    /// The drive below is the loop's *whole* write surface — `enter_zoom`, the
    /// keys, `on_paste`, the frame counter, `leave_zoom`. Everything else the
    /// loop does takes `&self` and draws. So a document that rebuilds
    /// identically after all of them is the guarantee, not a sample of it.
    #[tokio::test]
    async fn a_zoom_round_trip_leaves_the_transcript_exactly_as_it_was() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        chat.messages.push(crate::tui::chat::UiMessage {
            role: crate::tui::chat::Role::User,
            text: "first".into(),
            at: 0,
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.messages.push(crate::tui::chat::UiMessage {
            role: crate::tui::chat::Role::Assistant,
            text: "an answer".into(),
            at: 0,
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        // Two settled messages already flushed into scrollback: the interesting
        // state, because a cursor that moved would reprint or skip.
        chat.flushed_segments = 3;
        chat.build_rows(100);
        let before: Vec<String> = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
        let before_messages = chat.messages.len();
        let before_flushed = chat.flushed_segments;
        let before_tail = chat.tail_start;

        let home = chat.buffers.active().clone();
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        let mut state = ZoomState::default();
        on_key(
            &mut chat,
            &mut state,
            KeyCode::Char('h'),
            KeyModifiers::NONE,
            20,
        );
        chat.on_paste("ello");
        on_key(
            &mut chat,
            &mut state,
            KeyCode::Enter,
            KeyModifiers::NONE,
            20,
        );
        on_key(
            &mut chat,
            &mut state,
            KeyCode::PageUp,
            KeyModifiers::NONE,
            20,
        );
        on_key(
            &mut chat,
            &mut state,
            KeyCode::BackTab,
            KeyModifiers::NONE,
            20,
        );
        chat.tick = chat.tick.wrapping_add(1);
        on_key(&mut chat, &mut state, KeyCode::Esc, KeyModifiers::NONE, 20);
        chat.leave_zoom(home);

        assert_eq!(chat.messages.len(), before_messages, "no message was added");
        assert_eq!(
            chat.flushed_segments, before_flushed,
            "the flush cursor did not move, so nothing reprints and nothing is skipped"
        );
        assert_eq!(chat.tail_start, before_tail);
        chat.build_rows(100);
        let after: Vec<String> = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
        assert_eq!(after, before, "the document rebuilds identically");
    }

    /// And what happened while it was open still happens: a message that landed
    /// during the zoom is drawn on the way out, once, in order. The zoom drains
    /// nothing, which is exactly why.
    #[tokio::test]
    async fn what_arrives_during_a_zoom_arrives_after_it() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        chat.build_rows(100);
        let home = chat.buffers.active().clone();
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        chat.messages.push(crate::tui::chat::UiMessage {
            role: crate::tui::chat::Role::Assistant,
            text: "landed while you were away".into(),
            at: 0,
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.leave_zoom(home);
        chat.build_rows(100);
        let text = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            text.matches("landed while you were away").count(),
            1,
            "once, not twice: {text}"
        );
    }

    // -- the chrome --------------------------------------------------------

    /// The composer is addressing somebody, and says so: the box and the `❯`
    /// take the viewed identity's colour, which is the tint D90 spent on the
    /// DM buffer and D103 took back.
    #[test]
    fn the_composer_wears_the_colour_of_whoever_it_is_addressing() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        let plain = crate::tui::chrome::prompt(&chat, 60);
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        let tinted = crate::tui::chrome::prompt(&chat, 60);
        assert_ne!(
            format!("{plain:?}"),
            format!("{tinted:?}"),
            "the composer looks different when it is talking to somebody else"
        );
    }

    /// The hint row says which meaning `esc` has right now, and carries the
    /// viewed agent's mode where CC carries it.
    #[test]
    fn the_hint_row_says_which_esc_this_is() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        chat.enter_zoom(ZoomTarget::Agent("scout".into()));
        let idle = crate::tui::chrome::zoom_chrome(&chat, 100);
        let text = crate::tui::statics::layout(vec![crate::tui::statics::Block::live(idle)])
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("esc to return"), "{text}");
        assert!(!text.contains("esc stops the run"), "{text}");
        assert!(
            text.contains("shift + tab to cycle mode"),
            "an agent has a mode to cycle: {text}"
        );
    }

    /// A room has neither a permission mode nor a place on the roster, so its
    /// row says the one key that means anything there — and the tree's leader
    /// row is not lit, because `@main` is not what is on screen either.
    #[test]
    fn a_room_zoom_advertises_only_the_key_it_has() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        seed_room(&chat, "build", &["scout"]);
        chat.open_agent_tree();
        chat.enter_zoom(ZoomTarget::Room("build".into()));
        let text = crate::tui::statics::layout(vec![crate::tui::statics::Block::live(
            crate::tui::chrome::zoom_chrome(&chat, 100),
        )])
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
        assert!(text.contains("esc to return"), "{text}");
        assert!(!text.contains("cycle mode"), "{text}");
        assert!(!text.contains("to switch"), "{text}");
        assert!(
            text.contains("┌─ @main"),
            "the leader row is dim while somewhere else has the screen: {text}"
        );
        assert!(
            key(&mut chat, KeyCode::BackTab, KeyModifiers::NONE) == Action::None
                && chat.zoom_permission_mode().is_none(),
            "and shift+tab has nothing to cycle"
        );
    }
}
