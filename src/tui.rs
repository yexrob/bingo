use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
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
use crate::permission::PermissionMode;
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
    /// 非致命警告（如 MCP 连接失败），显示在边框与分隔线之间。
    Warning(String),
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
    let warn_events = events.clone();
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
        on_warning: Box::new(move |message| {
            let _ = warn_events.send(UiEvent::Warning(message));
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
    output_tokens: u64,
    tick: u64,
    warnings: Vec<String>,
    user: String,
    cwd: String,
    pending_ask: Option<(PermissionRequest, oneshot::Sender<DialogAction>)>,
    processor: MarkdownProcessor,
    renderer: StreamMarkdownRenderer,
    reply_cache: HashMap<String, Vec<Line<'static>>>,
    width: usize,
    scroll: u16,
    auto_scroll: bool,
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
            output_tokens: 0,
            tick: 0,
            warnings: Vec::new(),
            user: std::env::var("USER").unwrap_or_else(|_| "user".to_string()),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            pending_ask: None,
            processor: MarkdownProcessor::default(),
            renderer: StreamMarkdownRenderer::new(80),
            reply_cache: HashMap::new(),
            width: 80,
            scroll: 0,
            auto_scroll: true,
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
                    // 占位 thinking：端点延迟推送 delta 时（DeepSeek 常达数十秒），
                    // 运行态行立即可见，用户能感知"正在思考"。
                    let mut hint = Activity::new(ActivityKind::Thinking(Thinking {
                        state: ThinkingState::Running,
                        duration_ms: 0,
                        digest: None,
                        stage: thinking_stage(self.messages.len()),
                        tokens: None,
                    }));
                    hint.expand_hint = Some("ctrl+o to expand".to_string());
                    if let Some(i) = self.stream_msg {
                        self.messages[i].activities.push(hint);
                    }
                }
                UiEvent::TextDelta(text) => {
                    if let Some(i) = self.stream_msg {
                        self.messages[i].text.push_str(&text);
                    }
                }
                UiEvent::ThinkingDelta(thinking) => {
                    if let Some(i) = self.stream_msg {
                        // 多轮 thinking 各自成块：最后一轮还在流（末尾是
                        // thinking）则续写；工具轮之后的 delta 开新块。
                        let last_is_thinking = self.messages[i]
                            .activities
                            .last()
                            .is_some_and(|a| matches!(a.kind, ActivityKind::Thinking(_)));
                        if last_is_thinking {
                            self.thinking_buf.push_str(&thinking);
                            let content = vec![Line::from(self.thinking_buf.clone())];
                            if let Some(hint) = self.messages[i]
                                .activities
                                .iter_mut()
                                .find(|a| matches!(a.kind, ActivityKind::Thinking(_)))
                            {
                                hint.set_content(content);
                            }
                        } else {
                            // 工具轮后的新一段思考。DeepSeek 兼容层偶发把
                            // 同一段 thinking 在 tool_use 前后各发一遍：
                            // 内容与上一轮相同则视为重复，不新开块。
                            let dup = thinking == self.thinking_buf
                                || self.messages[i]
                                    .activities
                                    .iter()
                                    .rev()
                                    .find(|a| matches!(a.kind, ActivityKind::Thinking(_)))
                                    .is_some_and(|a| {
                                        a.content.first().is_some_and(|l| l.to_string() == thinking)
                                    });
                            if dup {
                                continue;
                            }
                            // 清掉从未收到 delta 的空占位，然后新开一块（排在工具行之后）。
                            self.thinking_buf = thinking.clone();
                            self.messages[i].activities.retain(|a| {
                                !(matches!(a.kind, ActivityKind::Thinking(_))
                                    && a.content.is_empty())
                            });
                            let mut hint = Activity::new(ActivityKind::Thinking(Thinking {
                                state: ThinkingState::Running,
                                duration_ms: self.tick * 33,
                                digest: None,
                                stage: thinking_stage(self.messages.len()),
                                tokens: None,
                            }));
                            hint.set_content(vec![Line::from(thinking.clone())]);
                            hint.expand_hint = Some("ctrl+o to expand".to_string());
                            self.messages[i].activities.push(hint);
                        }
                    }
                }
                UiEvent::OutputTokens(_tokens) => {}
                UiEvent::ToolStart { name } => {
                    let name: &'static str = Box::leak(name.into_boxed_str());
                    let mut hint = Activity::new(ActivityKind::Tool(ToolCall::running(
                        name, "",
                    )));
                    hint.expand_hint = Some("ctrl+o to expand".to_string());
                    if let Some(i) = self.stream_msg {
                        self.messages[i].activities.push(hint);
                    }
                }
                UiEvent::ToolDone(done) => {
                    let Some(i) = self.stream_msg else {
                        return;
                    };
                    for hint in &mut self.messages[i].activities {
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
                    self.output_tokens = 0;
                    // 原位收尾：thinking 在它发生的位置转完成态（不重排到回复之后）；
                    // 从未收到 delta 的空占位直接移除（避免出现无内容的空行）。
                    if let Some(i) = self.stream_msg {
                        self.messages[i].activities.retain(|a| {
                            !(matches!(a.kind, ActivityKind::Thinking(_))
                                && a.content.is_empty())
                        });
                        for hint in &mut self.messages[i].activities {
                            if let ActivityKind::Thinking(t) = &mut hint.kind
                                && t.state == ThinkingState::Running
                            {
                                t.state = ThinkingState::Done;
                                t.duration_ms = self.tick * 33;
                                hint.expanded = false;
                            }
                        }
                    }
                    self.stream_msg = None;
                }
                UiEvent::Warning(message) => {
                    if !self.warnings.iter().any(|w| w == &message) {
                        self.warnings.push(message);
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

fn center(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.to_string();
    }
    let pad = (width - len) / 2;
    format!("{}{}", " ".repeat(pad), text)
}

fn column_row(
    theme: &Theme,
    left_w: usize,
    right_w: usize,
    left: Option<(String, ratatui::style::Color)>,
    right: Option<(String, ratatui::style::Color)>,
) -> Line<'static> {
    let mut spans = Vec::new();
    let (l_text, l_color) = left.unwrap_or_else(|| (String::new(), theme.dim().fg.unwrap_or(theme.text)));
    let l_len = l_text.chars().count();
    spans.push(Span::styled(l_text, ratatui::style::Style::default().fg(l_color)));
    spans.push(Span::styled(
        format!("{}│", " ".repeat(left_w.saturating_sub(l_len))),
        theme.dim(),
    ));
    let mut r_len = 0;
    if let Some((r_text, r_color)) = right {
        r_len = r_text.chars().count();
        spans.push(Span::styled(r_text, ratatui::style::Style::default().fg(r_color)));
    }
    spans.push(Span::styled(
        " ".repeat(right_w.saturating_sub(r_len)),
        theme.dim(),
    ));
    Line::from(spans)
}

/// 欢迎面板（启动横幅，1:1 对齐 Claude Code）：左栏 logo/欢迎/身份，
/// 右栏 Tips 与 What's new。
fn welcome_rows(
    theme: &Theme,
    user: &str,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
) -> Vec<Line<'static>> {
    let left_w = width * 3 / 5;
    let right_w = width.saturating_sub(left_w + 1);
    let accent = theme.tool_running;
    let mut rows = Vec::new();

    let logo = ["    ▐▛█▜▌", "   ▝▜███▛▘", "     ▘ ▘"];
    for line in logo {
        rows.push(column_row(
            theme,
            left_w,
            right_w,
            Some((center(line, left_w), theme.text)),
            None,
        ));
    }
    rows.push(column_row(theme, left_w, right_w, None, None));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        Some((center(&format!("Welcome back {user}!"), left_w), theme.text)),
        Some(("Tips for getting started".to_string(), accent)),
    ));
    rows.push(column_row(theme, left_w, right_w, None, None));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        Some((center(&format!("{model} · {mode}"), left_w), theme.dim().fg.unwrap_or(theme.text))),
        Some(("Enter 发送 · Esc 切换输入".to_string(), theme.text)),
    ));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        Some((center(user, left_w), theme.text)),
        Some(("ctrl+o 展开/折叠工具输出".to_string(), theme.text)),
    ));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        Some((center(cwd, left_w), theme.dim().fg.unwrap_or(theme.text))),
        Some(("MCP 服务配置在 settings.json".to_string(), theme.text)),
    ));
    rows.push(column_row(theme, left_w, right_w, None, Some(("─".repeat(right_w).to_string(), theme.dim().fg.unwrap_or(theme.text)))));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        None,
        Some(("What's new".to_string(), accent)),
    ));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        None,
        Some(("流式主循环 · Tool 协议 · 权限门".to_string(), theme.text)),
    ));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        None,
        Some(("Hooks · MCP · 子代理 · 自动记忆".to_string(), theme.text)),
    ));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        None,
        Some(("transcript 持久化 · --continue".to_string(), theme.text)),
    ));
    rows
}

/// Claude Code 风格的思考阶段俏皮词。
const THINKING_WORDS: [&str; 12] = [
    "Bootstrapping",
    "Razzle-dazzling",
    "Hashing",
    "Pondering",
    "Wrangling",
    "Synthesizing",
    "Mulling",
    "Churning",
    "Digesting",
    "Concocting",
    "Scheming",
    "Weaving",
];

fn thinking_stage(seed: usize) -> &'static str {
    THINKING_WORDS[seed % THINKING_WORDS.len()]
}

/// 欢迎卡片行（带 ╭╮ 边框），作为滚动内容的一部分——消息增长时随流上移。
fn welcome_card_rows(
    theme: &Theme,
    user: &str,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
) -> Vec<Line<'static>> {
    let gray = ratatui::style::Style::default().fg(ratatui::style::Color::Gray);
    let title = format!(" bingo v0.1.0 · {model} ");
    let title_len = title.chars().count();
    let mut rows = Vec::new();
    rows.push(Line::from(Span::styled(
        format!(
            "╭{}{}╮",
            title,
            "─".repeat(width.saturating_sub(title_len + 2))
        ),
        gray,
    )));
    let inner_w = width.saturating_sub(2);
    for line in welcome_rows(theme, user, model, mode, cwd, inner_w) {
        let mut spans = vec![Span::styled("│", gray)];
        spans.extend(line.spans);
        spans.push(Span::styled("│", gray));
        rows.push(Line::from(spans));
    }
    rows.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
        gray,
    )));
    rows
}

fn permission_mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::BypassPermissions => "bypassPermissions",
        PermissionMode::DontAsk => "dontAsk",
        PermissionMode::Plan => "plan",
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

        let warn_height = if self.warnings.is_empty() { 0 } else { 1 } as u16;
        // 警告行固定在最顶部（不随消息滚动）
        let warn_y = area.y;
        if warn_height > 0 {
            let warn = self.warnings.first().cloned().unwrap_or_default();
            buf.set_string(
                area.x,
                warn_y,
                format!(" ⚠ {warn}"),
                ratatui::style::Style::default().fg(self.theme.warning),
            );
        }

        // 消息区：欢迎卡片 + 消息作为同一滚动流（卡片随消息增长上移滚出）
        let msg_top = warn_y + warn_height;
        let msg_bottom_limit = area.height.saturating_sub(2); // 分隔线 + 输入
        let spinner = rsmarkdown_tui::activities::spinner(self.tick);
        let mut rows: Vec<Line<'static>> = Vec::new();
        rows.extend(welcome_card_rows(
            &self.theme,
            &self.user,
            &self.session.model,
            permission_mode_label(self.session.permission_mode),
            &self.cwd,
            area.width as usize,
        ));
        for i in 0..self.messages.len() {
            match self.messages[i].role {
                Role::User => {
                    rows.push(Line::from(vec![
                        Span::styled("❯ ", self.theme.tool_running()),
                        Span::styled(self.messages[i].text.clone(), self.theme.text),
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
                    let reply = render(&self.messages[i].text);
                    for (j, line) in reply.into_iter().enumerate() {
                        if j == 0 {
                            let mut spans = vec![Span::styled(
                                "⏺ ",
                                ratatui::style::Style::default().fg(self.theme.claude),
                            )];
                            spans.extend(line.spans);
                            rows.push(Line::from(spans));
                        } else {
                            rows.push(line);
                        }
                    }
                }
            }
        }

        // 消息区高度：跟随内容，不超过可用空间
        let needed = rows.len() as u16;
        let max_msg_h = msg_bottom_limit.saturating_sub(msg_top);
        let msg_h = needed.min(max_msg_h).max(1);
        let msg_rect = Rect {
            x: area.x,
            y: msg_top,
            width: area.width,
            height: msg_h,
        };

        let sep_y = msg_rect.y + msg_h;
        for x in 0..area.width {
            buf.set_string(
                area.x + x,
                sep_y,
                "─",
                ratatui::style::Style::default().fg(ratatui::style::Color::Gray),
            );
        }

        let caret = if self.typing { '▋' } else { ' ' };
        let input_line = Line::from(vec![
            Span::styled("❯ ", self.theme.tool_running()),
            Span::styled(self.input.clone(), self.theme.text),
            Span::styled(caret.to_string(), self.theme.tool_running()),
        ]);
        buf.set_line(area.x, sep_y + 1, &input_line, area.width);

        // 消息滚动（内容超出消息区时）
        let total = rows.len() as u16;
        let max_scroll = total.saturating_sub(msg_h);
        if self.auto_scroll {
            self.scroll = max_scroll;
        }
        let scroll = self.scroll.min(max_scroll);
        self.scroll = scroll;
        if scroll == max_scroll {
            self.auto_scroll = true;
        }
        for (y, line) in rows
            .iter()
            .skip(scroll as usize)
            .take(msg_h as usize)
            .enumerate()
        {
            buf.set_line(msg_rect.x, msg_rect.y + y as u16, line, msg_rect.width);
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
                    KeyCode::Char('j') | KeyCode::Down => {
                        self.auto_scroll = false;
                        self.scroll = self.scroll.saturating_add(1);
                        true
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.auto_scroll = false;
                        self.scroll = self.scroll.saturating_sub(1);
                        true
                    }
                    KeyCode::PageDown => {
                        self.auto_scroll = false;
                        self.scroll = self.scroll.saturating_add(10);
                        true
                    }
                    KeyCode::PageUp => {
                        self.auto_scroll = false;
                        self.scroll = self.scroll.saturating_sub(10);
                        true
                    }
                    KeyCode::Char('g') => {
                        self.auto_scroll = false;
                        self.scroll = 0;
                        true
                    }
                    KeyCode::Char('G') => {
                        self.auto_scroll = true;
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

/// 启动 TUI 会话。draw/event 崩溃时恢复终端并报告（不裸退）。
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

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = run_tui(&mut app);
    }));
    if let Err(payload) = result {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture
        );
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown panic".to_string());
        eprintln!("[bingo] TUI panicked: {message}");
        eprintln!(
            "[bingo] backtrace:\n{}",
            std::backtrace::Backtrace::force_capture()
        );
    }
    Ok(())
}

#[allow(dead_code)]
fn _assert_send(_: Pin<Box<dyn std::future::Future<Output = bool> + Send>>) {}
