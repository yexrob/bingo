//! 实体全屏视图（ctrl+g 选择器打开）：
//!
//! - **agent 对话视图**：该实例的完整消息历史（❯ 指令 / 正文 / ⏺ 工具行）
//!   + 运行中的流式活尾，只读，↑↓ 滚动。
//! - **频道房间**：微信式群聊——他人靠左（名签 + 气泡），`user`（你）
//!   靠右；底部输入行，Enter 以 `user` 身份发言（与 Post 工具同一投递
//!   路径，照常唤醒成员）。渲染即已读：你看着屏幕，serial 校验不会弹你。
//!
//! 两个视图都跑在交替屏（alternate screen）里：inline 的 write-once
//! scrollback 决定了不存在"就地换内容"，交替屏是可自由重绘的画布，
//! Esc 返回后主屏原样恢复（app 层再走一次确定性重画兜底）。

use std::io::stdout;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::api::types::{ContentBlock, Message, Role};
use crate::channels::{ChannelMessage, USER_NAME};
use crate::tui::chat::{Chat, EntityOpen, Row};
use crate::tui::line::{text_width, wrap_words, Line, SegStyle};
use crate::tui::theme::Theme;
use crate::tui::view;

/// 房间气泡的最大内容宽（终端宽的 3/5，微信比例）。
fn bubble_width(width: usize) -> usize {
    (width * 3 / 5).clamp(12, width.saturating_sub(6))
}

/// agent 对话视图内容行：历史 Message + 流式活尾。
pub fn agent_view_rows(
    history: &[Message],
    live: Option<&str>,
    width: usize,
    theme: &Theme,
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let wrap_w = width.saturating_sub(4).max(8);
    let push_gap = |rows: &mut Vec<Row>| {
        if !rows.is_empty() {
            rows.push(Row::new(Line::empty()));
        }
    };
    for msg in history {
        for block in &msg.content {
            match (msg.role, block) {
                (Role::User, ContentBlock::Text { text }) => {
                    push_gap(&mut rows);
                    for (i, para) in text.lines().enumerate() {
                        let lines = wrap_words(para, wrap_w.saturating_sub(2));
                        for (j, l) in lines.iter().enumerate() {
                            let prefix = if i == 0 && j == 0 { "❯ " } else { "  " };
                            rows.push(Row::new(Line::styled(
                                format!("{prefix}{l}"),
                                SegStyle::fg(theme.subtle),
                            )));
                        }
                        if lines.is_empty() {
                            rows.push(Row::new(Line::empty()));
                        }
                    }
                }
                (Role::Assistant, ContentBlock::Text { text }) => {
                    push_gap(&mut rows);
                    for para in text.lines() {
                        let lines = wrap_words(para, wrap_w);
                        for l in &lines {
                            rows.push(Row::new(Line::styled(
                                l.clone(),
                                SegStyle::fg(theme.text),
                            )));
                        }
                        if lines.is_empty() {
                            rows.push(Row::new(Line::empty()));
                        }
                    }
                }
                (Role::Assistant, ContentBlock::ToolUse { name, input, .. }) => {
                    push_gap(&mut rows);
                    let glyph = crate::tui::activities::tool_glyph(name);
                    let shown = crate::tui::activities::display_tool_name(name);
                    let summary = crate::query::summarize_input(name, input);
                    let head = if summary.is_empty() {
                        format!("{glyph}{shown}")
                    } else {
                        format!("{glyph}{shown}({summary})")
                    };
                    rows.push(Row::new(Line::styled(
                        crate::tui::chat::one_line(&head, width.saturating_sub(2)),
                        SegStyle::fg(theme.inactive),
                    )));
                }
                // 工具结果与思考不进视图（噪声；要细看用主 transcript 的展开）。
                _ => {}
            }
        }
    }
    if let Some(live) = live
        && !live.trim().is_empty()
    {
        rows.push(Row::new(Line::empty()));
        for para in live.lines() {
            for l in wrap_words(para, wrap_w) {
                rows.push(Row::new(Line::styled(l, SegStyle::fg(theme.text))));
            }
        }
        rows.push(Row::new(Line::styled(
            "✻ 生成中…".to_string(),
            SegStyle::fg(theme.thinking).italic(),
        )));
    }
    if rows.is_empty() {
        rows.push(Row::new(Line::styled(
            "（还没有完成的回合；运行中的产出会在回合结束后入档）".to_string(),
            SegStyle::fg(theme.inactive),
        )));
    }
    rows
}

/// 频道房间内容行：微信式气泡。`user`（你）靠右、user_message_bg；
/// 其他成员靠左、名签 + code_block_bg 气泡；同发件人连续消息合并名签。
pub fn room_rows(log: &[ChannelMessage], width: usize, theme: &Theme) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    if log.is_empty() {
        rows.push(Row::new(Line::styled(
            "（还没有消息；在下方输入，Enter 发送）".to_string(),
            SegStyle::fg(theme.inactive),
        )));
        return rows;
    }
    let bw = bubble_width(width);
    let mut prev_from: Option<&str> = None;
    for msg in log {
        let mine = msg.from == USER_NAME;
        if !rows.is_empty() {
            rows.push(Row::new(Line::empty()));
        }
        if !mine && prev_from != Some(msg.from.as_str()) {
            rows.push(Row::new(Line::styled(
                format!("  {}", msg.from),
                SegStyle::fg(theme.inactive),
            )));
        }
        let bg = if mine {
            theme.user_message_bg
        } else {
            theme.code_block_bg
        };
        for para in msg.text.lines() {
            let lines = wrap_words(para, bw);
            let lines = if lines.is_empty() {
                vec![String::new()]
            } else {
                lines
            };
            for l in lines {
                let body = format!(" {l} ");
                let mut row = Line::empty();
                if mine {
                    let pad = width
                        .saturating_sub(2)
                        .saturating_sub(text_width(&body));
                    row.push_styled(" ".repeat(pad), SegStyle::fg(theme.text));
                } else {
                    row.push_styled("  ".to_string(), SegStyle::fg(theme.text));
                }
                row.push_styled(body, SegStyle::fg(theme.text).with_bg(bg));
                rows.push(Row::new(row));
            }
        }
        prev_from = Some(msg.from.as_str());
    }
    rows
}

/// 视图头部（title + 分隔线）。
fn header_rows(title: String, width: usize, theme: &Theme) -> Vec<Row> {
    vec![
        Row::new(Line::styled(
            crate::tui::chat::one_line(&title, width),
            SegStyle::fg(theme.text).bold(),
        )),
        Row::new(Line::styled(
            "─".repeat(width.min(500)),
            SegStyle::fg(theme.inactive),
        )),
    ]
}

/// 全屏实体模态：交替屏内自绘循环，Esc/ctrl+c 返回。
/// `already_alt`：fullscreen 宿主本就在交替屏内，不再嵌套进出。
pub async fn run_entity_modal(
    chat: &mut Chat,
    events: &mut EventStream,
    open: EntityOpen,
    already_alt: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !already_alt {
        execute!(stdout(), EnterAlternateScreen)?;
    }
    let result = modal_loop(chat, events, &open).await;
    if !already_alt {
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
    result
}

async fn modal_loop(
    chat: &mut Chat,
    events: &mut EventStream,
    open: &EntityOpen,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    let mut ticker = tokio::time::interval(Duration::from_millis(33));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // 距底部的滚动偏移（0 = 贴底跟随最新）。
    let mut scroll_up: usize = 0;
    let mut input = String::new();
    let mut flash: Option<String> = None;

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    match key.code {
                        KeyCode::Esc => break,
                        KeyCode::Char('c') if ctrl => break,
                        KeyCode::Up => scroll_up = scroll_up.saturating_add(1),
                        KeyCode::Down => scroll_up = scroll_up.saturating_sub(1),
                        KeyCode::PageUp => scroll_up = scroll_up.saturating_add(10),
                        KeyCode::PageDown => scroll_up = scroll_up.saturating_sub(10),
                        _ => {
                            if let EntityOpen::Channel(name) = open {
                                match key.code {
                                    KeyCode::Enter => {
                                        let text = input.trim().to_string();
                                        if !text.is_empty() {
                                            input.clear();
                                            flash = post_as_user(&chat.session, name, &text);
                                            scroll_up = 0;
                                        }
                                    }
                                    KeyCode::Backspace => {
                                        input.pop();
                                    }
                                    KeyCode::Char('u') if ctrl => input.clear(),
                                    KeyCode::Char(c) if !c.is_control() && !ctrl => {
                                        input.push(c);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Some(Ok(Event::Paste(text))) => {
                    if matches!(open, EntityOpen::Channel(_)) {
                        input.push_str(&text);
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
            _ = ticker.tick() => {
                // 后台 agent 的事件照常消化（通知/watch 行进 chat 状态），
                // 模态退出后主视图是最新的。
                chat.tick();
                let _ = chat.drain_all();
            }
        }

        let theme = chat.theme.clone();
        let session = chat.session.clone();
        terminal.draw(|frame| {
            let area = frame.area();
            let width = area.width as usize;
            let height = area.height as usize;
            let (header, content, footer, cursor_col) = match open {
                EntityOpen::Agent(name) => {
                    let (history, live, state) = session
                        .agents
                        .view_of(name)
                        .unwrap_or((Vec::new(), None, crate::agents::AgentState::Stopped));
                    let header = header_rows(
                        format!("◉ {name} · {}", state.label()),
                        width,
                        &theme,
                    );
                    let content =
                        agent_view_rows(&history, live.as_deref(), width, &theme);
                    let footer = vec![Row::new(Line::styled(
                        "esc 返回 · ↑↓ 滚动".to_string(),
                        SegStyle::fg(theme.inactive),
                    ))];
                    (header, content, footer, None)
                }
                EntityOpen::Channel(name) => {
                    let log = session.channels.log_of(name);
                    // 渲染即已读：serial 校验以屏幕为准，你永远"见过最新"。
                    if let Some(last) = log.last() {
                        session.channels.mark_seen(USER_NAME, name, last.seq);
                    }
                    let info = session.channels.info(name);
                    let title = match &info {
                        Some(i) => format!(
                            "◇ #{name} · {} · {} 条 · 成员 {}{}",
                            i.mode.label(),
                            i.seq,
                            i.members.join(", "),
                            if i.frozen { " · 已冻结" } else { "" }
                        ),
                        None => format!("◇ #{name}（已不存在）"),
                    };
                    let header = header_rows(title, width, &theme);
                    let content = room_rows(&log, width, &theme);
                    let mut footer = Vec::new();
                    if let Some(f) = &flash {
                        footer.push(Row::new(Line::styled(
                            crate::tui::chat::one_line(&format!("⚠ {f}"), width),
                            SegStyle::fg(theme.warning),
                        )));
                    }
                    let prompt = format!("> {input}");
                    let col = text_width(&prompt).min(width.saturating_sub(1));
                    footer.push(Row::new(Line::styled(
                        crate::tui::chat::one_line(&prompt, width),
                        SegStyle::fg(theme.text),
                    )));
                    footer.push(Row::new(Line::styled(
                        "enter 发送（以 user 身份）· esc 返回 · ↑↓ 滚动".to_string(),
                        SegStyle::fg(theme.inactive),
                    )));
                    (header, content, footer, Some(col))
                }
            };
            let viewport = height
                .saturating_sub(header.len())
                .saturating_sub(footer.len())
                .max(1);
            // 贴底 + 向上滚动偏移（clamp 不越顶）。
            let max_up = content.len().saturating_sub(viewport);
            let up = scroll_up.min(max_up);
            let start = content.len().saturating_sub(viewport + up);
            let slice: Vec<Row> = content
                .iter()
                .skip(start)
                .take(viewport)
                .cloned()
                .collect();
            let buf = frame.buffer_mut();
            let fg = theme.text;
            view::render_rows(&header, fg, buf, area);
            if let Ok(y) = u16::try_from(header.len()) {
                view::render_rows(
                    &slice,
                    fg,
                    buf,
                    ratatui::layout::Rect::new(
                        area.x,
                        area.y.saturating_add(y),
                        area.width,
                        area.height.saturating_sub(y),
                    ),
                );
            }
            let footer_y = height.saturating_sub(footer.len());
            if let Ok(y) = u16::try_from(footer_y) {
                view::render_rows(
                    &footer,
                    fg,
                    buf,
                    ratatui::layout::Rect::new(
                        area.x,
                        area.y.saturating_add(y),
                        area.width,
                        area.height.saturating_sub(y),
                    ),
                );
            }
            // 频道输入行光标（footer 倒数第二行是输入行）。
            if let Some(col) = cursor_col {
                let input_y = height.saturating_sub(2);
                if let (Ok(x), Ok(y)) = (u16::try_from(col), u16::try_from(input_y)) {
                    frame.set_cursor_position((area.x + x, area.y + y));
                }
            }
        })?;
    }
    Ok(())
}

/// 以 `user` 身份发言（与 Post 工具同一条投递/唤醒路径）。
/// 返回 None = 成功；Some = 提示文本（弹回/错误）。
fn post_as_user(
    session: &Arc<crate::query::Session>,
    channel: &str,
    text: &str,
) -> Option<String> {
    match crate::tool::channel::deliver_post(
        session,
        &session.watch,
        USER_NAME,
        channel,
        text,
    ) {
        Ok(crate::tool::channel::PostDelivery::Sent { .. }) => None,
        // 渲染即已读之下几乎不会弹回；万一撞上（同帧竞争），提示重发。
        Ok(crate::tool::channel::PostDelivery::Stale { .. }) => {
            Some("频道刚有新消息，请看完后重发".to_string())
        }
        Err(e) => Some(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(from: &str, seq: u64, text: &str) -> ChannelMessage {
        ChannelMessage {
            seq,
            from: from.into(),
            text: text.into(),
        }
    }

    /// 微信式布局：user 靠右（行前大段空白），他人靠左带名签，
    /// 同发件人连续消息不重复名签。
    #[test]
    fn room_rows_align_user_right_and_others_left() {
        let theme = Theme::dark();
        let log = vec![
            msg("法官", 1, "3号请发言"),
            msg("法官", 2, "补充：限时一分钟"),
            msg("user", 3, "都停一下"),
            msg("scout", 4, "收到"),
        ];
        let rows = room_rows(&log, 80, &theme);
        let texts: Vec<String> = rows.iter().map(|r| r.line.plain_text()).collect();
        // 名签：法官出现一次（连续消息合并），scout 一次。
        assert_eq!(
            texts.iter().filter(|t| t.trim() == "法官").count(),
            1,
            "{texts:?}"
        );
        assert!(texts.iter().any(|t| t.trim() == "scout"));
        // 他人靠左：气泡行以两格缩进开头。
        assert!(texts.iter().any(|t| t.starts_with("   3号请发言")
            || t.starts_with("   补充")
            || t.starts_with("  ") && t.contains("3号请发言")));
        // user 靠右：行前空白把内容推到右侧（起始列 > 半宽）。
        let mine = rows
            .iter()
            .find(|r| r.line.plain_text().contains("都停一下"))
            .unwrap_or_else(|| panic!("有 user 行"));
        let text = mine.line.plain_text();
        let lead = text.len() - text.trim_start().len();
        assert!(lead > 40, "右对齐（前导空白 {lead}）: {text:?}");
        // user 气泡用 user_message_bg，他人用 code_block_bg。
        assert!(mine
            .line
            .segs
            .iter()
            .any(|s| s.style.bg == Some(theme.user_message_bg)));
        let other = rows
            .iter()
            .find(|r| r.line.plain_text().contains("收到"))
            .unwrap_or_else(|| panic!("有 scout 行"));
        assert!(other
            .line
            .segs
            .iter()
            .any(|s| s.style.bg == Some(theme.code_block_bg)));
        // 空日志：占位提示。
        assert!(room_rows(&[], 80, &theme)[0]
            .line
            .plain_text()
            .contains("还没有消息"));
    }

    /// agent 对话视图：❯ 指令、正文、⏺ 工具行、活尾与空态。
    #[test]
    fn agent_view_renders_history_and_live_tail() {
        let theme = Theme::dark();
        let history = vec![
            Message::user_text("调研 D27 的取舍"),
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
        let rows = agent_view_rows(&history, Some("正在写第二段"), 80, &theme);
        let joined: Vec<String> = rows.iter().map(|r| r.line.plain_text()).collect();
        assert!(joined.iter().any(|t| t.starts_with("❯ 调研 D27")), "{joined:?}");
        assert!(joined.iter().any(|t| t.contains("⏺ Bash($ rg lazy)")));
        assert!(joined.iter().any(|t| t.contains("结论：懒落盘正确。")));
        assert!(joined.iter().any(|t| t.contains("正在写第二段")));
        assert!(joined.iter().any(|t| t.contains("生成中")));
        // 空历史：占位。
        let empty = agent_view_rows(&[], None, 80, &theme);
        assert!(empty[0].line.plain_text().contains("还没有完成的回合"));
    }
}
