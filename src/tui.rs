use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
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
            _ => {}
        }),
        on_tool_done: Box::new(move |done| {
            let _ = tool_events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
                name: done.name.clone(),
                summary: done.summary.clone(),
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
                    self.activities.push(Activity::new(ActivityKind::Thinking(
                        Thinking {
                            state: ThinkingState::Running,
                            duration_ms: 0,
                            digest: (!digest.is_empty()).then_some(digest),
                            stage: "thinking",
                        },
                    )));
                }
                UiEvent::ToolStart { name } => {
                    let name: &'static str = Box::leak(name.into_boxed_str());
                    self.activities.push(Activity::new(ActivityKind::Tool(
                        ToolCall::running(name, ""),
                    )));
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
                        }
                    }
                }
                UiEvent::TurnEnd => {
                    self.busy = false;
                    self.stream_msg = None;
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
                Ok(()) => {
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

        let [chat_area, input_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

        let caret = if self.typing { '▋' } else { ' ' };
        let input_line = Line::from(vec![
            Span::styled("you › ", self.theme.tool_running()),
            Span::styled(self.input.clone(), self.theme.text()),
            Span::styled(caret.to_string(), self.theme.tool_running()),
        ]);
        buf.set_line(0, input_area.y, &input_line, input_area.width);

        let spinner = rsmarkdown_tui::activities::spinner(0);
        let mut rows: Vec<Line<'static>> = Vec::new();
        for i in 0..self.messages.len() {
            match self.messages[i].role {
                Role::User => {
                    rows.push(Line::from(vec![
                        Span::styled("you ", self.theme.tool_running()),
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
        let scroll = self.scroll.min(total.saturating_sub(chat_area.height));
        self.scroll = scroll;
        for (y, line) in rows
            .iter()
            .skip(scroll as usize)
            .take(chat_area.height as usize)
            .enumerate()
        {
            buf.set_line(0, chat_area.y + y as u16, line, chat_area.width);
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
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn status(&self) -> String {
        if self.busy {
            "working…".to_string()
        } else {
            "idle".to_string()
        }
    }

    fn hints(&self) -> &'static str {
        "type + Enter to send · Esc toggles input"
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
    run_tui(&mut app)?;
    Ok(())
}

#[allow(dead_code)]
fn _assert_send(_: Pin<Box<dyn std::future::Future<Output = bool> + Send>>) {}
