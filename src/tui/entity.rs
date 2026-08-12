//! Full-screen workspace view (opened directly with ctrl+g), wearing the
//! Slack shape defined in [`crate::tui::slack`] (D43, narrowed to one pane by
//! D47): a channel log or an instance's history as a Slack message list, with a
//! composer that speaks as `user` into a channel and as the hub into a DM.
//!
//! There is no rail and no sidebar. Navigation is the ctrl+K switcher and
//! alt+↑/↓ — a list of conversations is worth less than the columns it costs when
//! ctrl+K reaches any of them in two keystrokes, and the view paints no surface
//! colours of its own, so the terminal's own background shows through.
//!
//! This module owns the terminal and the state machine; every row on screen is
//! built by the pure functions in [`crate::tui::slack`]. It runs on the
//! alternate screen because inline's write-once scrollback rules out "swapping
//! content in place"; Esc restores the main screen as it was (the app layer
//! re-draws deterministically once more as a safety net).

use std::io::stdout;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

use crate::channels::USER_NAME;
use crate::query::Session;
use crate::tui::chat::{Chat, EntityOpen, Row};
use crate::tui::slack::{
    self, Avatars, ChannelItem, Conv, DmItem, Focus, Palette, Post, Snapshot, Switcher, Workspace,
};
use crate::tui::{avatar, gfx, view};

/// Sample everything the view needs from the session. Instances seen for the
/// first time are seeded as read: a workspace you have never opened shouldn't
/// greet you with an unread badge for every turn that already happened.
fn snapshot(session: &Arc<Session>, ws: &mut Workspace, workspace: &str) -> Snapshot {
    let statuses = session.agents.list();
    let channels: Vec<ChannelItem> = session
        .channels
        .list()
        .into_iter()
        .map(|c| ChannelItem {
            unread: c
                .seq
                .saturating_sub(session.channels.seen_of(USER_NAME, &c.name)),
            name: c.name,
            seq: c.seq,
            frozen: c.frozen,
            mode: c.mode,
            members: c.members,
        })
        .collect();
    let mut dms: Vec<DmItem> = Vec::new();
    for status in statuses {
        let conv = Conv::Dm(status.name.clone());
        let seq = dm_seq(session, &status.name);
        if !ws.knows(&conv) {
            ws.mark_read(&conv, seq);
        }
        dms.push(DmItem {
            unread: seq.saturating_sub(ws.read_cursor(&conv)),
            name: status.name,
            state: status.state,
            model: status.model,
            thinking: status.thinking,
            description: status.description,
            last_active: status.last_active.elapsed(),
        });
    }
    Snapshot {
        workspace: workspace.to_string(),
        main_model: session.runtime.model.borrow().clone(),
        main_thinking: session.runtime.thinking.borrow().clone(),
        channels,
        dms,
    }
}

/// The blueprint's contribution to the view: what the workspace is called, and
/// which portrait each crew member wears. Read once when the view opens rather
/// than per frame — a file read at 30fps to answer a question whose answer is a
/// committed file is waste, and the crew does not change while you are looking at
/// it.
fn blueprint(session: &Arc<Session>, images: bool) -> (String, Avatars) {
    let cwd = session.cwd();
    let tree = crate::team::load_team_tree(&cwd).ok().flatten();
    let name = tree
        .as_ref()
        .map(|t| t.root().def.name.clone())
        .or_else(|| cwd.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "bingo".to_string());
    // Every team in the chart, not just the root: a member's face is pinned in its own
    // blueprint, and a face that only resolves for the root team is a face missing from
    // exactly the rooms a chart exists to hold.
    let pinned = tree
        .iter()
        .flat_map(|t| t.members())
        .filter_map(|(_, m)| {
            let id = m.avatar.as_deref()?;
            Some((m.name.clone(), avatar::index_of_id(id)?))
        })
        .collect();
    (name, Avatars { images, pinned })
}

/// Read cursor unit for a DM: completed history messages.
fn dm_seq(session: &Arc<Session>, name: &str) -> u64 {
    session
        .agents
        .view_of(name)
        .map(|(history, _, _, _, _)| history.len() as u64)
        .unwrap_or(0)
}

/// The open conversation as posts, plus its current sequence and the index the
/// unread divider belongs at.
fn conversation(session: &Arc<Session>, ws: &Workspace, conv: &Conv) -> (Vec<Post>, u64, usize) {
    let cursor = ws.entry_cursor(conv);
    match conv {
        Conv::Channel(name) => {
            let log = session.channels.log_of(name);
            let seq = log.last().map(|m| m.seq).unwrap_or(0);
            let divider = log.iter().position(|m| m.seq > cursor).unwrap_or(log.len());
            (slack::channel_posts(&log, USER_NAME), seq, divider)
        }
        Conv::Dm(name) => {
            let (history, stamps, live, in_flight, _) = session.agents.view_of(name).unwrap_or((
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                crate::agents::AgentState::Stopped,
            ));
            let pending = session.agents.pending_of(name);
            let seq = history.len() as u64;
            let read_upto = (cursor as usize).min(history.len());
            let divider = slack::dm_posts(
                &history[..read_upto],
                &stamps,
                &[],
                &[],
                &[],
                name,
                USER_NAME,
            )
            .len();
            let posts = slack::dm_posts(
                &history, &stamps, &in_flight, &live, &pending, name, USER_NAME,
            );
            (posts, seq, divider)
        }
    }
}

/// Send the draft. `None` = accepted; `Some` = a notice to show above the
/// composer.
fn send(session: &Arc<Session>, conv: &Conv, text: &str) -> Option<String> {
    match conv {
        Conv::Channel(name) => {
            match crate::tool::channel::deliver_post(session, &session.watch, USER_NAME, name, text)
            {
                Ok(crate::tool::channel::PostDelivery::Sent { .. }) => None,
                // With render-as-read this rarely bounces; if it does (same-frame
                // race), prompt the user to resend.
                Ok(crate::tool::channel::PostDelivery::Stale { .. }) => {
                    Some("the channel got new messages; read them and resend".to_string())
                }
                Err(e) => Some(e),
            }
        }
        Conv::Dm(name) => match session.agents.deliver(name, text, Vec::new(), None) {
            Ok(_) => {
                crate::tool::agent::flush_agent_inbox(session, &session.watch);
                None
            }
            Err(e) => Some(e),
        },
    }
}

fn apply_open(ws: &mut Workspace, open: EntityOpen) {
    match open {
        EntityOpen::Workspace => {}
        EntityOpen::Agent(name) => ws.select(Conv::Dm(name)),
    }
    ws.focus = Focus::Composer;
    ws.switcher = None;
}

/// Full-screen entity modal: a self-drawing loop on the alternate screen,
/// Esc/ctrl+c returns.
/// `already_alt`: the fullscreen host is already on the alternate screen, so
/// there is no nested enter/leave.
pub async fn run_entity_modal(
    chat: &mut Chat,
    events: &mut EventStream,
    open: EntityOpen,
    already_alt: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !already_alt && let Err(e) = execute!(stdout(), EnterAlternateScreen, EnableMouseCapture) {
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        return Err(e.into());
    }
    let result = modal_loop(chat, events, open).await;
    if !already_alt {
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
    result
}

async fn modal_loop(
    chat: &mut Chat,
    events: &mut EventStream,
    open: EntityOpen,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    let mut ticker = tokio::time::interval(Duration::from_millis(33));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // The workspace state lives on Chat so read cursors and the open
    // conversation survive leaving and re-entering the view.
    let mut ws = std::mem::take(&mut chat.slack);
    apply_open(&mut ws, open);

    // Avatars are ordinary kitty images: transmitted once per portrait, then
    // placed by the cells the message list paints.
    let image_cap = chat.image_cap;
    let motion_off = chat.session.settings.motion.as_deref() == Some("off")
        || std::env::var_os("BINGO_NO_MOTION").is_some();
    let mut transmits = gfx::Transmits::default();
    let mut scroll_content: Option<(Conv, usize)> = None;
    let (workspace, avatars) = blueprint(&chat.session, image_cap.is_some());

    loop {
        let session = chat.session.clone();
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    let snap = snapshot(&session, &mut ws, &workspace);
                    if !handle_key(&session, &mut ws, &snap, key) {
                        break;
                    }
                }
                Some(Ok(Event::Mouse(mouse))) => {
                    let _ = handle_mouse(&mut ws, mouse);
                }
                Some(Ok(Event::Paste(text))) => {
                    match &mut ws.switcher {
                        Some(sw) => sw.query.push_str(&text),
                        None => ws.composer.push_str(&text),
                    }
                }
                // A resize may purge the terminal's image store, so the avatars
                // have to go out again with the repainted placeholder cells.
                Some(Ok(Event::Resize(_, _))) => transmits.reset(),
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
            _ = ticker.tick() => {
                // Background agent events keep being drained (notices/watch
                // still advance chat state), so the main view is up to date
                // once the modal exits.
                chat.tick();
                let _ = chat.drain_all();
            }
        }

        let theme = chat.theme.clone();
        let pal = Palette::new(&theme);
        let snap = snapshot(&session, &mut ws, &workspace);
        ws.sync(&snap);
        // Rendering counts as read, for channels the same way it always has
        // (serial validation goes by what's on screen) and for instances so the
        // sidebar badge clears while you watch.
        if let Some(conv) = ws.open.clone() {
            let (_, seq, _) = conversation(&session, &ws, &conv);
            if let Conv::Channel(name) = &conv {
                session.channels.mark_seen(USER_NAME, name, seq);
            }
            ws.mark_read(&conv, seq);
        }

        // Portraits for everyone the open conversation shows, sent before the
        // frame that places them (order does not matter to the protocol, but a
        // transmit that never happens shows an empty chip).
        if let Some(cap) = image_cap
            && let Some(conv) = ws.open.clone()
        {
            let (posts, _, _) = conversation(&session, &ws, &conv);
            let faces: Vec<usize> = posts.iter().map(|p| avatars.index_of(&p.from)).collect();
            let bytes = avatar::transmits(&faces, &cap, &mut transmits);
            let _ = crate::tui::term::write_transmits(terminal.backend_mut(), &bytes);
        }

        terminal.draw(|frame| {
            let main = slack::layout(frame.area());
            let height = main.height as usize;
            let buf = frame.buffer_mut();
            let fg = pal.main_text;
            let width = main.width as usize;
            let Some(conv) = ws.open.clone() else {
                view::render_rows(&slack::empty_pane_rows(&pal, width, height), fg, buf, main);
                return;
            };

            let header = slack::header_rows(&snap, &conv, &pal, width);
            let (token_rate, context_usage) = match &conv {
                Conv::Dm(name) => (
                    session
                        .agents
                        .token_rate_label(name, std::time::Instant::now(), motion_off),
                    session.agents.context_usage(name),
                ),
                Conv::Channel(_) => (None, None),
            };
            let (composer, caret) = slack::composer_rows(
                &ws,
                &conv,
                &pal,
                width,
                token_rate.as_deref(),
                context_usage,
            );
            let viewport = height
                .saturating_sub(header.len())
                .saturating_sub(composer.len())
                .max(1);
            let (posts, _, divider) = conversation(&session, &ws, &conv);
            let content = match conv {
                Conv::Dm(_) => {
                    slack::dm_message_rows(&posts, divider, &pal, width, &avatars, &theme)
                }
                Conv::Channel(_) => slack::message_rows(&posts, divider, &pal, width, &avatars),
            };

            preserve_scroll(&mut ws, &mut scroll_content, &conv, content.len());
            clamp_scroll(&mut ws, content.len(), viewport);
            let start = content.len().saturating_sub(viewport + ws.scroll_up);
            let mut slice: Vec<Row> = content.iter().skip(start).take(viewport).cloned().collect();
            while slice.len() < viewport {
                slice.push(slack::blank_row());
            }

            view::render_rows(&header, fg, buf, main);
            render_at(&slice, fg, buf, main, header.len());
            render_at(&composer, fg, buf, main, header.len() + viewport);

            let caret_cell = match (&ws.switcher, caret) {
                (Some(sw), _) => {
                    let (rows, _) = slack::switcher_rows(&snap, sw, &pal, width);
                    let top = header.len() + viewport.saturating_sub(rows.len() + 1);
                    render_at(&rows, fg, buf, main, top);
                    Some((top + 1, 5 + crate::tui::line::text_width(&sw.query)))
                }
                (None, Some((r, c))) if ws.focus == Focus::Composer => {
                    Some((header.len() + viewport + r, c))
                }
                _ => None,
            };
            if let Some((r, c)) = caret_cell
                && let (Ok(y), Ok(x)) = (u16::try_from(r), u16::try_from(c))
            {
                frame.set_cursor_position((main.x + x.min(main.width), main.y + y));
            }
        })?;
    }
    chat.slack = ws;
    Ok(())
}

/// Draw `rows` `offset` rows down inside `pane`.
fn render_at(
    rows: &[Row],
    fg: ratatui::style::Color,
    buf: &mut ratatui::buffer::Buffer,
    pane: Rect,
    offset: usize,
) {
    let Ok(offset) = u16::try_from(offset) else {
        return;
    };
    if offset >= pane.height {
        return;
    }
    view::render_rows(
        rows,
        fg,
        buf,
        Rect::new(
            pane.x,
            pane.y.saturating_add(offset),
            pane.width,
            pane.height.saturating_sub(offset),
        ),
    );
}

fn preserve_scroll(
    ws: &mut Workspace,
    previous: &mut Option<(Conv, usize)>,
    conv: &Conv,
    content_rows: usize,
) {
    match previous {
        Some((open, rows)) if open == conv => {
            if ws.scroll_up > 0 {
                ws.scroll_up = ws
                    .scroll_up
                    .saturating_add(content_rows.saturating_sub(*rows));
            }
            *rows = content_rows;
        }
        _ => *previous = Some((conv.clone(), content_rows)),
    }
}

fn clamp_scroll(ws: &mut Workspace, content_rows: usize, viewport: usize) {
    ws.scroll_up = ws.scroll_up.min(content_rows.saturating_sub(viewport));
}

const WHEEL_ROWS: usize = 3;

fn handle_mouse(ws: &mut Workspace, mouse: MouseEvent) -> bool {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            ws.scroll_up = ws.scroll_up.saturating_add(WHEEL_ROWS);
            true
        }
        MouseEventKind::ScrollDown => {
            ws.scroll_up = ws.scroll_up.saturating_sub(WHEEL_ROWS);
            true
        }
        _ => false,
    }
}

/// Route one key. Returns `false` to leave the view.
fn handle_key(
    session: &Arc<Session>,
    ws: &mut Workspace,
    snap: &Snapshot,
    key: crossterm::event::KeyEvent,
) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if ctrl && key.code == KeyCode::Char('c') {
        return false;
    }
    if ctrl && key.code == KeyCode::Char('k') {
        ws.switcher = match ws.switcher {
            Some(_) => None,
            None => Some(Switcher::default()),
        };
        return true;
    }
    if let Some(sw) = &mut ws.switcher {
        let matches = slack::switcher_matches(snap, &sw.query);
        match key.code {
            KeyCode::Esc => ws.switcher = None,
            KeyCode::Up => sw.sel = sw.sel.saturating_sub(1),
            KeyCode::Down => sw.sel = (sw.sel + 1).min(matches.len().saturating_sub(1)),
            KeyCode::Backspace => {
                sw.query.pop();
                sw.sel = 0;
            }
            KeyCode::Enter => {
                let pick = matches.get(sw.sel).cloned();
                ws.switcher = None;
                if let Some(conv) = pick {
                    ws.select(conv);
                    ws.focus = Focus::Composer;
                }
            }
            KeyCode::Char(c) if !ctrl && !c.is_control() => {
                sw.query.push(c);
                sw.sel = 0;
            }
            _ => {}
        }
        return true;
    }

    // Slack's alt+↑/↓ jumps conversations from wherever the focus is.
    if alt && matches!(key.code, KeyCode::Up | KeyCode::Down) {
        ws.step(snap, if key.code == KeyCode::Up { -1 } else { 1 });
        return true;
    }
    match key.code {
        KeyCode::Esc => return false,
        KeyCode::Tab => ws.focus = ws.focus.next(),
        KeyCode::PageUp => ws.scroll_up = ws.scroll_up.saturating_add(10),
        KeyCode::PageDown => ws.scroll_up = ws.scroll_up.saturating_sub(10),
        _ => match ws.focus {
            Focus::Messages => match key.code {
                KeyCode::Up => ws.scroll_up = ws.scroll_up.saturating_add(1),
                KeyCode::Down => ws.scroll_up = ws.scroll_up.saturating_sub(1),
                _ => {}
            },
            Focus::Composer => match key.code {
                KeyCode::Up => ws.scroll_up = ws.scroll_up.saturating_add(1),
                KeyCode::Down => ws.scroll_up = ws.scroll_up.saturating_sub(1),
                KeyCode::Enter => {
                    let text = ws.composer.trim().to_string();
                    if let (false, Some(conv)) = (text.is_empty(), ws.open.clone()) {
                        ws.composer.clear();
                        ws.flash = send(session, &conv, &text);
                        ws.scroll_up = 0;
                    }
                }
                KeyCode::Backspace => {
                    ws.composer.pop();
                }
                KeyCode::Char('u') if ctrl => ws.composer.clear(),
                KeyCode::Char(c) if !ctrl && !c.is_control() => ws.composer.push(c),
                _ => {}
            },
        },
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentState;
    use crate::channels::ChannelMode;
    use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};

    fn snap() -> Snapshot {
        Snapshot {
            workspace: "bingo".into(),
            main_model: "main-model".into(),
            main_thinking: Some("high".into()),
            channels: vec![ChannelItem {
                name: "dev-room".into(),
                seq: 2,
                unread: 1,
                frozen: false,
                mode: ChannelMode::Serial,
                members: vec!["main".into(), "user".into()],
            }],
            dms: vec![DmItem {
                name: "scout".into(),
                state: AgentState::Idle,
                model: "test-model".into(),
                thinking: None,
                description: "recon".into(),
                last_active: Duration::ZERO,
                unread: 0,
            }],
        }
    }

    fn press(ws: &mut Workspace, snap: &Snapshot, code: KeyCode, mods: KeyModifiers) -> bool {
        handle_key(
            &crate::tui::test_util::test_session(),
            ws,
            snap,
            KeyEvent::new(code, mods),
        )
    }

    #[test]
    fn direct_agent_open_selects_that_dm_without_losing_workspace_navigation() {
        let mut ws = Workspace::default();
        ws.select(Conv::Channel("dev-room".into()));
        ws.switcher = Some(Switcher::default());
        apply_open(&mut ws, EntityOpen::Agent("scout".into()));
        assert_eq!(ws.open, Some(Conv::Dm("scout".into())));
        assert_eq!(ws.focus, Focus::Composer);
        assert!(ws.switcher.is_none());

        apply_open(&mut ws, EntityOpen::Workspace);
        assert_eq!(
            ws.open,
            Some(Conv::Dm("scout".into())),
            "Ctrl+G keeps the workspace's last conversation"
        );
    }

    #[test]
    fn mouse_wheel_scrolls_three_rows_from_either_focus() {
        let wheel = |kind| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let mut ws = Workspace::default();

        assert!(handle_mouse(&mut ws, wheel(MouseEventKind::ScrollUp)));
        assert_eq!(ws.scroll_up, 3);
        ws.scroll_up = usize::MAX;
        clamp_scroll(&mut ws, 12, 5);
        assert_eq!(ws.scroll_up, 7, "overscroll clamps to the oldest row");
        assert!(handle_mouse(&mut ws, wheel(MouseEventKind::ScrollDown)));
        assert_eq!(ws.scroll_up, 4);

        ws.scroll_up = 4;
        let conv = Conv::Channel("dev-room".into());
        let mut previous = Some((conv.clone(), 12));
        preserve_scroll(&mut ws, &mut previous, &conv, 15);
        assert_eq!(ws.scroll_up, 7, "new rows preserve the viewed content");
        ws.scroll_up = 0;
        preserve_scroll(&mut ws, &mut previous, &conv, 18);
        assert_eq!(ws.scroll_up, 0, "the latest view stays pinned");

        ws.scroll_up = 0;
        ws.focus = Focus::Messages;
        assert!(handle_mouse(&mut ws, wheel(MouseEventKind::ScrollUp)));
        assert_eq!(ws.scroll_up, 3);
        assert!(!handle_mouse(&mut ws, wheel(MouseEventKind::Moved)));
    }

    #[test]
    fn esc_and_ctrl_c_leave_the_view() {
        let snap = snap();
        let mut ws = Workspace::default();
        assert!(!press(&mut ws, &snap, KeyCode::Esc, KeyModifiers::NONE));
        assert!(!press(
            &mut ws,
            &snap,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ));
    }

    #[test]
    fn tab_walks_the_two_focuses_and_typing_lands_in_the_composer() {
        let snap = snap();
        let mut ws = Workspace::default();
        assert_eq!(ws.focus, Focus::Composer, "open means ready to type");
        press(&mut ws, &snap, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(ws.focus, Focus::Messages);
        press(&mut ws, &snap, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(ws.focus, Focus::Composer, "only two focuses remain");

        press(&mut ws, &snap, KeyCode::Char('h'), KeyModifiers::NONE);
        press(&mut ws, &snap, KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(ws.composer, "hi");
        press(&mut ws, &snap, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(ws.composer, "h");
        press(&mut ws, &snap, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert!(ws.composer.is_empty());
    }

    /// With the sidebar gone, alt+↑/↓ is the only key-based way between
    /// conversations, so it has to reach every one of them from either focus.
    #[test]
    fn arrows_scroll_and_alt_arrows_walk_every_conversation() {
        let snap = snap();
        let mut ws = Workspace::default();
        ws.sync(&snap);
        press(&mut ws, &snap, KeyCode::Up, KeyModifiers::NONE);
        press(&mut ws, &snap, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(ws.scroll_up, 2, "↑↓ in the composer scroll the messages");
        press(&mut ws, &snap, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(ws.scroll_up, 0);

        // alt+↓ switches conversation from anywhere; the scroll resets with it.
        ws.scroll_up = 5;
        press(&mut ws, &snap, KeyCode::Down, KeyModifiers::ALT);
        assert_eq!(ws.open, Some(Conv::Dm("scout".into())));
        assert_eq!(ws.scroll_up, 0);

        ws.focus = Focus::Messages;
        press(&mut ws, &snap, KeyCode::Up, KeyModifiers::ALT);
        assert_eq!(
            ws.open,
            Some(Conv::Channel("dev-room".into())),
            "the messages area can switch conversations too"
        );
        press(&mut ws, &snap, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(ws.scroll_up, 1, "↑ in the messages area still scrolls");
    }

    #[test]
    fn the_switcher_filters_then_opens_what_you_picked() {
        let snap = snap();
        let mut ws = Workspace::default();
        ws.sync(&snap);
        press(&mut ws, &snap, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert!(ws.switcher.is_some());
        // Typing goes to the query, not the composer.
        press(&mut ws, &snap, KeyCode::Char('s'), KeyModifiers::NONE);
        press(&mut ws, &snap, KeyCode::Char('c'), KeyModifiers::NONE);
        assert_eq!(
            ws.switcher.as_ref().map(|s| s.query.clone()),
            Some("sc".into())
        );
        assert!(ws.composer.is_empty());
        press(&mut ws, &snap, KeyCode::Enter, KeyModifiers::NONE);
        assert!(ws.switcher.is_none());
        assert_eq!(ws.open, Some(Conv::Dm("scout".into())));
        assert_eq!(ws.focus, Focus::Composer);

        // Esc closes the switcher without leaving the view.
        press(&mut ws, &snap, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert!(press(&mut ws, &snap, KeyCode::Esc, KeyModifiers::NONE));
        assert!(ws.switcher.is_none());
    }

    /// The composer is the one place a human can speak into the runtime from
    /// this view: a channel post goes out as `user`, and a DM uses SendMessage's
    /// immediate dispatcher.
    #[tokio::test]
    async fn sending_reaches_channels_and_instances() {
        let session = crate::tui::test_util::test_session();
        session
            .channels
            .create("dev-room", vec!["scout".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("create channel: {e}"));
        assert_eq!(
            send(&session, &Conv::Channel("dev-room".into()), "everyone stop"),
            None
        );
        let log = session.channels.log_of("dev-room");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].from, USER_NAME);
        assert!(log[0].at > 0, "the landing time is stamped");

        // An unknown instance reports rather than silently swallowing the draft.
        let err = send(&session, &Conv::Dm("nobody".into()), "are you there");
        assert!(
            err.is_some_and(|e| e.contains("nobody")),
            "unknown instances must error"
        );
    }

    #[test]
    fn snapshot_carries_every_channel_members_engine() {
        let session = crate::tui::test_util::test_session();
        for (name, model, thinking) in [
            ("scout", "gpt-5.6-sol", Some("max")),
            ("qa", "claude-sonnet", Some("high")),
        ] {
            let member = crate::tui::test_util::test_session();
            let _ = member.runtime.model_tx.send(model.to_string());
            let _ = member
                .runtime
                .thinking_tx
                .send(thinking.map(str::to_string));
            session.agents.insert(
                name,
                crate::agents::AgentKind::Crew,
                None,
                name.to_string(),
                member,
            );
        }
        session
            .channels
            .create(
                "dev-team",
                vec!["scout".into(), "qa".into()],
                ChannelMode::Serial,
            )
            .unwrap_or_else(|e| panic!("{e}"));

        let mut ws = Workspace::default();
        let snap = snapshot(&session, &mut ws, "bingo");
        let header = crate::tui::slack::header_rows(
            &snap,
            &Conv::Channel("dev-team".into()),
            &crate::tui::slack::Palette::new(&crate::tui::theme::Theme::dark()),
            120,
        );
        let text = header
            .iter()
            .map(|row| row.line.plain_text())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("main: test-model/off"), "{text}");
        assert!(text.contains("scout: gpt-5.6-sol/max"), "{text}");
        assert!(text.contains("qa: claude-sonnet/high"), "{text}");
    }

    #[test]
    fn dm_history_becomes_posts_with_queued_drafts_last() {
        use crate::api::types::{ContentBlock, Message, Role};
        let history = vec![
            Message::user_text("research D27"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "rg lazy"}),
                    },
                    ContentBlock::Text {
                        text: "Conclusion: lazy flush is correct.".into(),
                    },
                ],
            },
        ];
        let posts = slack::dm_posts(
            &history,
            &[],
            &[],
            &[crate::agents::LiveBlock::Text(
                "writing the second paragraph".into(),
            )],
            &["read it again".to_string()],
            "scout",
            USER_NAME,
        );
        let kinds: Vec<_> = posts.iter().map(|p| (p.you, p.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                (true, slack::PostKind::Said),
                (false, slack::PostKind::Said),
                (true, slack::PostKind::Queued),
                (false, slack::PostKind::Typing),
            ],
            "{posts:?}"
        );
        assert_eq!(posts[1].text, "Conclusion: lazy flush is correct.");
        assert!(posts.iter().all(|post| !post.text.contains("Bash")));
    }
}
