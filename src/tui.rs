use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use rsmarkdown_core::{MarkdownProcessor, Renderer};
use rsmarkdown_tui::activities::{
    layout_activities, Activity, ActivityKind, Thinking, ThinkingState, ToolCall, ToolStatus,
};
use rsmarkdown_tui::app::App;
use rsmarkdown_tui::Component;
use rsmarkdown_tui::permission::{DialogAction, PermissionRequest};
use rsmarkdown_tui::renderer::theme::Theme;
use rsmarkdown_tui::renderer::StreamMarkdownRenderer;
use rsmarkdown_tui::{FooterBadge, run_tui};
use tokio::sync::{mpsc, oneshot};

use crate::api::types::StreamEvent;
use crate::query::{Session, ToolCallDone, UiHooks};

/// agent task → 组件的事件通道。
#[derive(Debug, Clone)]
pub enum UiEvent {
    TurnStart,
    TextDelta(String),
    ThinkingDelta(String),
    /// message_delta 的输出 token 累计值。
    OutputTokens(u64),
    ToolStart { name: String },
    ToolDone(ToolCallDone),
    TurnEnd,
    Error(String),
}

/// 权限询问：请求 + 结果回执。
pub type AskRequest = (PermissionRequest, oneshot::Sender<DialogAction>);

/// 把 query 的 UiHooks 接到 TUI 通道上。
pub fn tui_hooks(
    events: mpsc::UnboundedSender<UiEvent>,
    asks: mpsc::UnboundedSender<AskRequest>,
) -> UiHooks {
    let tool_events = events.clone();
    UiHooks {
        on_event: Box::new(move |event| match event {
            StreamEvent::TextDelta { text, .. } => {
                let _ = events.send(UiEvent::TextDelta(text.clone()));
            }
            StreamEvent::ThinkingDelta { thinking, .. } => {
                let _ = events.send(UiEvent::ThinkingDelta(thinking.clone()));
            }
            StreamEvent::ToolUseStart { name, .. } => {
                let _ = events.send(UiEvent::ToolStart { name: name.clone() });
            }
            StreamEvent::StopReason { output_tokens: Some(tokens), .. } => {
                let _ = events.send(UiEvent::OutputTokens(*tokens));
            }
            _ => {}
        }),
        on_tool_done: Box::new(move |done| {
            let _ = tool_events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
                name: done.name.clone(),
                summary: done.summary.clone(),
                output: done.output.clone(),
                is_error: done.is_error,
            }));
        }),
        ask: Box::new(move |tool_name, reason| {
            let request = PermissionRequest::new(
                format!("允许执行 {tool_name}"),
                reason,
                vec!["允许".to_string(), "拒绝".to_string()],
            );
            let (tx, rx) = oneshot::channel();
            if asks.send((request, tx)).is_err() {
                return Box::pin(async { false });
            }
            Box::pin(async move {
                matches!(rx.await, Ok(DialogAction::Confirm(0)))
            })
        }),
    }
}

/// 一条会话消息（用户或 assistant 文本 + assistant 活动提示）。
struct UiMessage {
    role: Role,
    text: String,
    activities: Vec<Activity>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

/// bingo 的聊天组件：消息流 + 活动提示 + 输入框 + 权限模态。
pub struct BingoChat {
    session: Arc<Session>,
    events: mpsc::UnboundedSender<UiEvent>,
    asks: mpsc::UnboundedSender<AskRequest>,
    events_rx: mpsc::UnboundedReceiver<UiEvent>,
    asks_rx: mpsc::UnboundedReceiver<AskRequest>,
    messages: Vec<UiMessage>,
    input: String,
    typing: bool,
    busy: bool,
    /// 当前 assistant 消息索引。
    stream_msg: Option<usize>,
    thinking_buf: String,
    activities: Vec<Activity>,
    /// 当前展开目标：None 表示最近的可展开活动。
    output_tokens: u64,
    tick: u64,
    pending_ask: Option<(PermissionRequest, oneshot::Sender<DialogAction>)>,
    processor: MarkdownProcessor,
    renderer: StreamMarkdownRenderer,
    reply_cache: HashMap<String, Vec<Line<'static>>>,
    width: usize,
    scroll: u16,
    theme: Theme,
}

impl BingoChat {
    pub fn new(
        session: Arc<Session>,
        events: mpsc::UnboundedSender<UiEvent>,
        events_rx: mpsc::UnboundedReceiver<UiEvent>,
        asks: mpsc::UnboundedSender<AskRequest>,
        asks_rx: mpsc::UnboundedReceiver<AskRequest>,
    ) -> Self {
        Self {
            session,
            events,
            asks,
            events_rx,
            asks_rx,
            messages: Vec::new(),
            input: String::new(),
            typing: true,
            busy: false,
            stream_msg: None,
            thinking_buf: String::new(),
            activities: Vec::new(),
            output_tokens: 0,
            tick: 0,
            pending_ask: None,
            processor: MarkdownProcessor::default(),
            renderer: StreamMarkdownRenderer::new(80),
            reply_cache: HashMap::new(),
            width: 80,
            scroll: 0,
            theme: Theme::dark(),
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                UiEvent::TurnStart => {
                    self.thinking_buf.clear();
                    self.messages.push(UiMessage {
                        role: Role::Assistant,
                        text: String::new(),
                        activities: Vec::new(),
                    });
                    self.stream_msg = Some(self.messages.len() - 1);
                    self.busy = true;
                }
                UiEvent::TextDelta(text) => {
                    if let Some(i) = self.stream_msg {
                        self.messages[i].text.push_str(&text);
                    }
                }
                UiEvent::ThinkingDelta(thinking) => {
                    self.thinking_buf.push_str(&thinking);
                    let digest = summarize(&self.thinking_buf);
                    self.activities
                        .retain(|a| !matches!(a.kind, ActivityKind::Thinking(_)));
                    let mut hint = Activity::new(ActivityKind::Thinking(Thinking {
                        state: ThinkingState::Running,
                        duration_ms: self.tick * 33,
                        digest: (!digest.is_empty()).then_some(digest),
                        stage: "thinking",
                        tokens: (self.output_tokens > 0).then_some(self.output_tokens),
                    }));
                    hint.set_content(vec![Line::from(self.thinking_buf.clone())]);
                    self.activities.push(hint);
                }
                UiEvent::OutputTokens(tokens) => {
                    self.output_tokens = tokens;
                    for hint in &mut self.activities {
                        if let ActivityKind::Thinking(t) = &mut hint.kind {
                            t.tokens = Some(tokens);
                        }
                    }
                }
                UiEvent::ToolStart { name } => {
                    let name: &'static str = Box::leak(name.into_boxed_str());
                    let mut hint = Activity::new(ActivityKind::Tool(ToolCall::running(
                        name, "",
                    )));
                    hint.expand_hint = Some("ctrl+o to expand".to_string());
                    self.activities.push(hint);
                }
                UiEvent::ToolDone(done) => {
                    for hint in &mut self.activities {
                        if let ActivityKind::Tool(call) = &mut hint.kind
                            && call.name == done.name.as_str()
                            && call.status == ToolStatus::Running
                        {
                            call.status = if done.is_error {
                                ToolStatus::Error
                            } else {
                                ToolStatus::Done
                            };
                            call.summary = done.summary.clone();
                            call.duration_ms = 0;
                            let content: Vec<Line<'static>> = done
                                .output
                                .lines()
                                .filter(|l| !l.trim().is_empty())
                                .take(4)
                                .map(|l| Line::from(l.to_string()))
                                .collect();
                            hint.set_content(content);
                        }
                    }
                }
                UiEvent::TurnEnd => {
                    self.busy = false;
                    self.stream_msg = None;
                    self.output_tokens = 0;
                    for hint in &mut self.activities {
                        if let ActivityKind::Thinking(t) = &mut hint.kind
                            && t.state == ThinkingState::Running
                        {
                            t.state = ThinkingState::Done;
                            t.duration_ms = self.tick * 33;
                            t.digest = Some(summarize(&self.thinking_buf));
                            hint.expanded = false;
                        }
                    }
                    if let Some(i) = self.messages.len().checked_sub(1) {
                        self.messages[i].activities = std::mem::take(&mut self.activities);
                    }
                }
                UiEvent::Error(message) => {
                    self.busy = false;
                    self.stream_msg = None;
                    if let Some(msg) = self.messages.pop() {
                        self.messages.push(UiMessage {
                            role: Role::Assistant,
                            text: format!("[error] {message}"),
                            activities: msg.activities,
                        });
                    }
                }
            }
        }
    }

    fn drain_asks(&mut self) {
        if self.pending_ask.is_none()
            && let Ok(request) = self.asks_rx.try_recv()
        {
            self.pending_ask = Some(request);
        }
    }

    /// 折叠/展开最近的可展开活动（ctrl+o）。
    fn toggle_recent_expand(&mut self) -> bool {
        let toggled = Self::toggle_in(&mut self.activities);
        if toggled {
            return true;
        }
        if let Some(i) = self.messages.len().checked_sub(1) {
            return Self::toggle_in(&mut self.messages[i].activities);
        }
        false
    }

    fn toggle_in(activities: &mut [Activity]) -> bool {
        if let Some(i) = activities.iter().rposition(|a| a.expanded) {
            activities[i].expanded = false;
            return true;
        }
        if let Some(i) = activities.iter().rposition(|a| a.expandable()) {
            activities[i].expanded = true;
            return true;
        }
        false
    }

    fn submit(&mut self) {
        let text = std::mem::take(&mut self.input);
        if text.trim().is_empty() || self.busy {
            self.input = text;
            return;
        }
        self.messages.push(UiMessage {
            role: Role::User,
            text: text.clone(),
            activities: Vec::new(),
        });
        self.busy = true;

        let session = self.session.clone();
        let events = self.events.clone();
        let asks = self.asks.clone();
        tokio::spawn(async move {
            let _ = events.send(UiEvent::TurnStart);
            let mut ui = tui_hooks(events.clone(), asks);
            let result = crate::query::run_query(&session, Vec::new(), &text, &mut ui).await;
            match result {
                Ok(messages) => {
                    let cwd = std::env::current_dir().unwrap_or_default();
                    crate::memory::extract_memory(&session, &messages, &session.home, &cwd)
                        .await;
                    let _ = events.send(UiEvent::TurnEnd);
                }
                Err(e) => {
                    let _ = events.send(UiEvent::Error(e.to_string()));
                }
            }
        });
    }

}

fn summarize(text: &str) -> String {
    let trimmed = text.trim();
    let first = trimmed.lines().next().unwrap_or(trimmed);
    let chars: Vec<char> = first.chars().collect();
    if chars.len() > 40 {
        chars[..40].iter().collect()
    } else {
        first.to_string()
    }
}

impl Component for BingoChat {
    fn title(&self) -> &str {
        "bingo"
    }

    fn draw(&mut self, area: Rect, buf: &mut Buffer) {
        self.drain_events();
        self.drain_asks();
        self.width = area.width as usize;

        // Claude Code 布局：消息区带边框 → 分隔线 → ❯ 输入行
        let input_height = 2u16;
        let bordered = area.height.saturating_sub(input_height);
        let [border_area, sep_area, input_area] = Layout::vertical([
            Constraint::Length(bordered),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(self.theme.dim())
            .title(Line::from(vec![
                Span::styled(" bingo v0.1.0", self.theme.tool_running()),
                Span::styled(format!(" · {}", self.session.model), self.theme.text()),
                Span::styled(" ", self.theme.text()),
            ]));
        let inner = block.inner(border_area);
        block.render(border_area, buf);

        for x in 0..sep_area.width {
            buf.set_string(
                sep_area.x + x,
                sep_area.y,
                "─",
                ratatui::style::Style::default().fg(self.theme.dim().fg.unwrap_or(
                    ratatui::style::Color::DarkGray,
                )),
            );
        }

        let caret = if self.typing { '▋' } else { ' ' };
        let input_line = Line::from(vec![
            Span::styled("❯ ", self.theme.tool_running()),
            Span::styled(self.input.clone(), self.theme.text()),
            Span::styled(caret.to_string(), self.theme.tool_running()),
        ]);
        buf.set_line(0, input_area.y, &input_line, input_area.width);

        let spinner = rsmarkdown_tui::activities::spinner(self.tick);
        let mut rows: Vec<Line<'static>> = Vec::new();
        for i in 0..self.messages.len() {
            match self.messages[i].role {
                Role::User => {
                    rows.push(Line::from(vec![
                        Span::styled("❯ ", self.theme.tool_running()),
                        Span::styled(self.messages[i].text.clone(), self.theme.text()),
                    ]));
                }
                Role::Assistant => {
                    let mut render = {
                        let processor = &mut self.processor;
                        let renderer = &mut self.renderer;
                        let cache = &mut self.reply_cache;
                        let width = self.width;
                        move |reply: &str| {
                            if reply.is_empty() {
                                return Vec::new();
                            }
                            if let Some(lines) = cache.get(reply) {
                                return lines.clone();
                            }
                            renderer.set_width(width);
                            let doc = processor.process_streaming(reply);
                            renderer.render(&doc);
                            let lines = renderer.lines().to_vec();
                            cache.insert(reply.to_string(), lines.clone());
                            lines
                        }
                    };
                    let (lines, _ranges) = layout_activities(
                        0,
                        rows.len() as u16,
                        &self.messages[i].activities,
                        spinner,
                        &self.theme,
                        &mut render,
                    );
                    rows.extend(lines);
                    rows.extend(render(&self.messages[i].text));
                }
            }
        }

        let total = rows.len() as u16;
        let scroll = self
            .scroll
            .min(total.saturating_sub(inner.height));
        self.scroll = scroll;
        for (y, line) in rows
            .iter()
            .skip(scroll as usize)
            .take(inner.height as usize)
            .enumerate()
        {
            buf.set_line(inner.x, inner.y + y as u16, line, inner.width);
        }
    }

    fn event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if self.typing {
                    match key.code {
                        KeyCode::Char(c)
                            if !c.is_control()
                                && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            self.input.push(c);
                            return true;
                        }
                        KeyCode::Backspace => {
                            self.input.pop();
                            return true;
                        }
                        KeyCode::Enter => {
                            self.submit();
                            return true;
                        }
                        _ => {}
                    }
                }
                match key.code {
                    KeyCode::Esc => {
                        self.typing = !self.typing;
                        true
                    }
                    KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.toggle_recent_expand();
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    fn status(&self) -> String {
        if self.busy {
            "working…".to_string()
        } else {
            "idle".to_string()
        }
    }

    fn hints(&self) -> &'static str {
        "Enter to send · Esc toggles input · ctrl+o expand"
    }

    fn footer_badges(&self) -> Vec<FooterBadge> {
        vec![FooterBadge::new(
            self.session.model.clone(),
            self.theme.tool_running(),
        )]
    }

    fn on_ask(&mut self) -> Option<PermissionRequest> {
        self.pending_ask
            .as_ref()
            .map(|(request, _)| request.clone())
    }

    fn on_dialog_closed(&mut self, action: DialogAction) {
        if let Some((_, tx)) = self.pending_ask.take() {
            let _ = tx.send(action);
        }
    }

    fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }
}

/// 启动 TUI 会话。
pub fn run_tui_session(session: Arc<Session>) -> Result<(), Box<dyn std::error::Error>> {
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (asks_tx, asks_rx) = mpsc::unbounded_channel();

    let mut app = App::new(vec![Box::new(BingoChat::new(
        session,
        events_tx,
        events_rx,
        asks_tx,
        asks_rx,
    ))]);
    app.set_status_bar(false);
    run_tui(&mut app)?;
    Ok(())
}

#[allow(dead_code)]
fn _assert_send(_: Pin<Box<dyn std::future::Future<Output = bool> + Send>>) {}
