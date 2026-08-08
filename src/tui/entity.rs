//! Full-screen workspace view (opened from the ctrl+g picker), wearing the
//! Slack shape defined in [`crate::tui::slack`] (D43, superseding D30's
//! single-conversation modal):
//!
//! - the **rail** switches between 主页 / 私信 / 动态;
//! - the **sidebar** lists 频道 (channels) and 私信 (subagent instances) with
//!   presence dots and unread badges;
//! - the **conversation pane** renders either a channel log or an instance's
//!   history as a Slack message list, with a composer that speaks as `user`
//!   into a channel and as the hub into a DM.
//!
//! This module owns the terminal and the state machine; every row on screen is
//! built by the pure functions in [`crate::tui::slack`]. It runs on the
//! alternate screen because inline's write-once scrollback rules out "swapping
//! content in place"; Esc restores the main screen as it was (the app layer
//! re-draws deterministically once more as a safety net).

use std::io::stdout;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
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
    self, ChannelItem, Conv, DmItem, Focus, Palette, Post, Snapshot, Switcher, Tab, Workspace,
};
use crate::tui::view;

/// Sample everything the view needs from the session. Instances seen for the
/// first time are seeded as read: a workspace you have never opened shouldn't
/// greet you with an unread badge for every turn that already happened.
fn snapshot(session: &Arc<Session>, ws: &mut Workspace) -> Snapshot {
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
    for status in session.agents.list() {
        let conv = Conv::Dm(status.name.clone());
        let seq = dm_seq(session, &status.name);
        if !ws.knows(&conv) {
            ws.mark_read(&conv, seq);
        }
        dms.push(DmItem {
            unread: seq.saturating_sub(ws.read_cursor(&conv)),
            name: status.name,
            state: status.state,
            description: status.description,
        });
    }
    Snapshot {
        workspace: workspace_name(session),
        channels,
        dms,
    }
}

/// Workspace name: the team's, falling back to the project directory.
fn workspace_name(session: &Arc<Session>) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Ok(Some(def)) = crate::team::load_team_file(&cwd) {
        return def.name;
    }
    let _ = session;
    cwd.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "bingo".to_string())
}

/// Read cursor unit for a DM: completed history messages.
fn dm_seq(session: &Arc<Session>, name: &str) -> u64 {
    session
        .agents
        .view_of(name)
        .map(|(history, _, _)| history.len() as u64)
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
            let (history, live, _) = session.agents.view_of(name).unwrap_or((
                Vec::new(),
                None,
                crate::agents::AgentState::Stopped,
            ));
            let pending = session.agents.pending_of(name);
            let seq = history.len() as u64;
            let read_upto = (cursor as usize).min(history.len());
            let divider = slack::dm_posts(&history[..read_upto], None, &[], name, USER_NAME).len();
            let posts = slack::dm_posts(&history, live.as_deref(), &pending, name, USER_NAME);
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
                    Some("频道刚有新消息，请看完后重发".to_string())
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
    if !already_alt {
        execute!(stdout(), EnterAlternateScreen)?;
    }
    let result = modal_loop(chat, events, open).await;
    if !already_alt {
        let _ = execute!(stdout(), LeaveAlternateScreen);
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

    // The workspace state lives on Chat so read cursors, the open conversation
    // and the collapsed sections survive leaving and re-entering the view.
    let mut ws = std::mem::take(&mut chat.slack);
    ws.select(match open {
        EntityOpen::Agent(name) => Conv::Dm(name),
        EntityOpen::Channel(name) => Conv::Channel(name),
    });
    ws.focus = Focus::Composer;
    ws.switcher = None;

    // Pane presence drives the Tab cycle; refreshed from the last frame.
    let mut has_rail = true;
    let mut has_sidebar = true;

    loop {
        let session = chat.session.clone();
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    let snap = snapshot(&session, &mut ws);
                    if !handle_key(&session, &mut ws, &snap, key, has_rail, has_sidebar) {
                        break;
                    }
                }
                Some(Ok(Event::Paste(text))) => {
                    match &mut ws.switcher {
                        Some(sw) => sw.query.push_str(&text),
                        None => ws.composer.push_str(&text),
                    }
                }
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
        let snap = snapshot(&session, &mut ws);
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

        let panes = slack::layout(terminal.get_frame().area());
        has_rail = panes.rail.is_some();
        has_sidebar = panes.sidebar.is_some();

        terminal.draw(|frame| {
            let area = frame.area();
            let panes = slack::layout(area);
            let height = area.height as usize;
            let buf = frame.buffer_mut();
            let fg = pal.main_text;

            if let Some(rail) = panes.rail {
                view::render_rows(&slack::rail_rows(&snap, &ws, &pal, height), fg, buf, rail);
            }
            if let Some(side) = panes.sidebar {
                let rows = slack::sidebar_rows(&snap, &ws, &pal, side.width as usize, height);
                view::render_rows(&rows, fg, buf, side);
            }

            let main = panes.main;
            let width = main.width as usize;
            let Some(conv) = ws.open.clone() else {
                view::render_rows(&slack::empty_pane_rows(&pal, width, height), fg, buf, main);
                return;
            };

            let header = slack::header_rows(&snap, &conv, &pal, width);
            let (composer, caret) = slack::composer_rows(&ws, &conv, &pal, width);
            let viewport = height
                .saturating_sub(header.len())
                .saturating_sub(composer.len())
                .max(1);
            let (posts, _, divider) = conversation(&session, &ws, &conv);
            let content = slack::message_rows(&posts, divider, &pal, width);

            // Bottom-anchored + scroll offset (clamped so it can't run past the top).
            let max_up = content.len().saturating_sub(viewport);
            let up = ws.scroll_up.min(max_up);
            let start = content.len().saturating_sub(viewport + up);
            let mut slice: Vec<Row> = content.iter().skip(start).take(viewport).cloned().collect();
            while slice.len() < viewport {
                slice.push(slack::blank_row(&pal));
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

/// Route one key. Returns `false` to leave the view.
fn handle_key(
    session: &Arc<Session>,
    ws: &mut Workspace,
    snap: &Snapshot,
    key: crossterm::event::KeyEvent,
    has_rail: bool,
    has_sidebar: bool,
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
        KeyCode::Tab => ws.focus = ws.focus.next(has_rail, has_sidebar),
        KeyCode::PageUp => ws.scroll_up = ws.scroll_up.saturating_add(10),
        KeyCode::PageDown => ws.scroll_up = ws.scroll_up.saturating_sub(10),
        _ => match ws.focus {
            Focus::Rail => match key.code {
                KeyCode::Up => ws.tab = prev_tab(ws.tab),
                KeyCode::Down => ws.tab = next_tab(ws.tab),
                KeyCode::Enter => ws.focus = Focus::Sidebar,
                _ => {}
            },
            Focus::Sidebar => match key.code {
                KeyCode::Up => ws.step(snap, -1),
                KeyCode::Down => ws.step(snap, 1),
                KeyCode::Left => ws.fold_section(snap, true),
                KeyCode::Right => ws.fold_section(snap, false),
                KeyCode::Enter => ws.focus = Focus::Composer,
                _ => {}
            },
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

fn next_tab(tab: Tab) -> Tab {
    match tab {
        Tab::Home => Tab::Dms,
        Tab::Dms => Tab::Activity,
        Tab::Activity => Tab::Home,
    }
}

fn prev_tab(tab: Tab) -> Tab {
    match tab {
        Tab::Home => Tab::Activity,
        Tab::Dms => Tab::Home,
        Tab::Activity => Tab::Dms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentState;
    use crate::channels::ChannelMode;
    use crossterm::event::KeyEvent;

    fn snap() -> Snapshot {
        Snapshot {
            workspace: "bingo".into(),
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
                description: "侦察".into(),
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
            true,
            true,
        )
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
    fn tab_walks_the_panes_and_typing_lands_in_the_composer() {
        let snap = snap();
        let mut ws = Workspace::default();
        assert_eq!(ws.focus, Focus::Composer, "打开即可打字");
        press(&mut ws, &snap, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(ws.focus, Focus::Rail);
        press(&mut ws, &snap, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(ws.focus, Focus::Sidebar);
        press(&mut ws, &snap, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(ws.focus, Focus::Messages);
        press(&mut ws, &snap, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(ws.focus, Focus::Composer);

        press(&mut ws, &snap, KeyCode::Char('h'), KeyModifiers::NONE);
        press(&mut ws, &snap, KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(ws.composer, "hi");
        press(&mut ws, &snap, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(ws.composer, "h");
        press(&mut ws, &snap, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert!(ws.composer.is_empty());

        // A narrow terminal drops the panes it can't show out of the cycle.
        assert_eq!(Focus::Composer.next(false, false), Focus::Messages);
        assert_eq!(Focus::Messages.next(false, false), Focus::Composer);
    }

    #[test]
    fn arrows_scroll_from_the_composer_and_navigate_from_the_sidebar() {
        let snap = snap();
        let mut ws = Workspace::default();
        ws.sync(&snap);
        press(&mut ws, &snap, KeyCode::Up, KeyModifiers::NONE);
        press(&mut ws, &snap, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(ws.scroll_up, 2, "输入框里的 ↑↓ 滚消息");
        press(&mut ws, &snap, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(ws.scroll_up, 0);

        // alt+↓ switches conversation from anywhere; the scroll resets with it.
        ws.scroll_up = 5;
        press(&mut ws, &snap, KeyCode::Down, KeyModifiers::ALT);
        assert_eq!(ws.open, Some(Conv::Dm("scout".into())));
        assert_eq!(ws.scroll_up, 0);

        ws.focus = Focus::Sidebar;
        press(&mut ws, &snap, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(ws.open, Some(Conv::Channel("dev-room".into())));
        press(&mut ws, &snap, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(ws.collapsed, [true, false], "← 折叠所在分组");
        press(&mut ws, &snap, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(ws.focus, Focus::Composer);
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

    #[test]
    fn the_rail_cycles_tabs() {
        let snap = snap();
        let mut ws = Workspace {
            focus: Focus::Rail,
            ..Workspace::default()
        };
        press(&mut ws, &snap, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(ws.tab, Tab::Dms);
        press(&mut ws, &snap, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(ws.tab, Tab::Activity);
        press(&mut ws, &snap, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(ws.tab, Tab::Dms);
    }

    /// The composer is the one place a human can speak into the runtime from
    /// this view: a channel post goes out as `user`, a DM queues on the
    /// instance and gets flushed at the turn boundary.
    #[tokio::test]
    async fn sending_reaches_channels_and_instances() {
        let session = crate::tui::test_util::test_session();
        session
            .channels
            .create("dev-room", vec!["scout".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("建频道: {e}"));
        assert_eq!(
            send(&session, &Conv::Channel("dev-room".into()), "都停一下"),
            None
        );
        let log = session.channels.log_of("dev-room");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].from, USER_NAME);
        assert!(log[0].at > 0, "落地时间被戳上");

        // An unknown instance reports rather than silently swallowing the draft.
        let err = send(&session, &Conv::Dm("nobody".into()), "在吗");
        assert!(err.is_some_and(|e| e.contains("nobody")), "未知实例要报错");
    }

    #[test]
    fn dm_history_becomes_posts_with_queued_drafts_last() {
        use crate::api::types::{ContentBlock, Message, Role};
        let history = vec![
            Message::user_text("调研 D27"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "rg lazy"}),
                    },
                    ContentBlock::Text {
                        text: "结论：懒落盘正确。".into(),
                    },
                ],
            },
        ];
        let posts = slack::dm_posts(
            &history,
            Some("正在写第二段"),
            &["再看一遍".to_string()],
            "scout",
            USER_NAME,
        );
        let kinds: Vec<_> = posts.iter().map(|p| (p.you, p.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                (true, slack::PostKind::Said),
                (false, slack::PostKind::Tool),
                (false, slack::PostKind::Said),
                (true, slack::PostKind::Queued),
                (false, slack::PostKind::Typing),
            ],
            "{posts:?}"
        );
        assert!(posts[1].text.contains("Bash"));
    }
}
