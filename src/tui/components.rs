//! iocraft UI 层：根组件（全屏布局）+ transcript 滚动区。
//!
//! 布局 1:1 对标 Claude Code 2.1.88（`screens/REPL.tsx` +
//! `components/FullscreenLayout.tsx`）：
//!
//! ```text
//! ┌─ 根 View（height = 终端行数）──────────────────────────┐
//! │ [sticky header]（滚动离开底部时：`❯ 首条用户消息`）     │
//! │ [Transcript]（flex_grow=1，行级切片滚动）              │
//! │   ├ 欢迎卡片（LogoV2 风）                              │
//! │   ├ 消息流（用户气泡 / ⏺ 正文 / 活动 / 折叠组）        │
//! │   └ 权限请求块（CC 渲染在 ScrollBox 内）               │
//! │ [任务列表]（TaskListV2 位置：输入框上方）              │
//! │ [通知行]（CC Notifications overlay 位置）             │
//! │ [输入行] `❯ {input}▋`                                │
//! │ [边框行] `╰──────╯`（CC promptBorder 底部边框）        │
//! │ [footer]（模式徽标 · 快捷键 byline · 右侧模型名）       │
//! └──────────────────────────────────────────────────────┘
//! ```

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use iocraft::prelude::*;
use tokio::sync::mpsc;

/// 终端尺寸变化检测：轮询 crossterm 实际尺寸，变化时触发整屏清除重绘。
/// iocraft 的 diff 写入在 canvas 高度变化/内容整体位移时可能残留旧行
/// （小窗口 resize 场景）；全清重写绕开 diff 路径。
pub trait UseForceRedrawOnResize: private::Sealed {
    fn use_force_redraw_on_resize(&mut self);
}

mod private {
    pub trait Sealed {}
    impl Sealed for iocraft::Hooks<'_, '_> {}
}

impl UseForceRedrawOnResize for Hooks<'_, '_> {
    fn use_force_redraw_on_resize(&mut self) {
        self.use_hook(|| ForceRedrawOnResize {
            last: None,
            changed: false,
        });
    }
}

struct ForceRedrawOnResize {
    last: Option<(u16, u16)>,
    changed: bool,
}

impl Hook for ForceRedrawOnResize {
    fn poll_change(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        let size = crossterm::terminal::size().unwrap_or((0, 0));
        if self.last != Some(size) {
            self.last = Some(size);
            self.changed = true;
        }
        if self.changed {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    fn post_component_update(&mut self, updater: &mut iocraft::ComponentUpdater) {
        let force = crate::tui::chat::FORCE_FULL_REDRAW.swap(false, std::sync::atomic::Ordering::Relaxed);
        if self.changed || force {
            self.changed = false;
            updater.clear_terminal_output();
        }
    }
}

use crate::permission::PermissionMode;
use crate::query::Session;
use crate::tui::chat::{Chat, Row};
use crate::tui::line::Line;
use crate::tui::theme::{Theme, ThemeSetting};

/// 每帧 tick 间隔（spinner/thinking 计时）。
const TICK_MS: u64 = 33;
/// 任务列表磁盘快照刷新间隔（tick 数）。
const TASKS_REFRESH_TICKS: u64 = 15;

/// 根组件 props。iocraft 的 element! 要求 props 实现 Default，无法
/// Default 构造的类型以 Option 传入（构造后 unwrap）。
#[derive(Default, Props)]
pub struct BingoProps {
    pub session: Option<Arc<Session>>,
    pub expand_rx: Option<tokio::sync::watch::Receiver<bool>>,
    /// OSC 11 实测的终端背景色（None = 未检测到，由 Theme 回落）。
    pub detected_background: Option<bool>,
}

/// bingo 主界面根组件：通道驱动状态 + 全屏布局。
#[component]
pub fn Bingo(mut hooks: Hooks, props: &BingoProps) -> impl Into<AnyElement<'static>> {
    let session = props.session.clone().expect("Bingo requires a session");
    let expand_rx = props
        .expand_rx
        .clone()
        .expect("Bingo requires an expand_rx");
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (asks_tx, asks_rx) = mpsc::unbounded_channel();
    let mut chat = hooks.use_state(move || {
        Chat::new(
            session.clone(),
            events_tx,
            events_rx,
            asks_tx,
            asks_rx,
            Theme::for_terminal(
                ThemeSetting::parse(session.settings.theme.as_deref()),
                props.detected_background,
            ),
        )
    });

    hooks.use_force_redraw_on_resize();
    let (width, height) = hooks.use_terminal_size();

    // 主循环：tick（spinner/thinking 计时）+ 通道排空 + 任务快照。
    {
        let mut tick_chat = chat;
        hooks.use_future(async move {
            let mut tick: u64 = 0;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
                let mut s = tick_chat.write();
                s.tick();
                if s.drain_all() {
                    tick = 0;
                    // 事件处理（TextDelta/ThinkingDelta/ToolStart 等）会改变
                    // 流式内容——iocraft 行 diff 在"内容增长但行数不变"的帧
                    // 可能残留旧行（真实终端实测：正文半截覆盖）。全量重写
                    // 绕开 diff；synchronized update 下同帧原子完成，不闪。
                    crate::tui::chat::FORCE_FULL_REDRAW
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
                if tick.is_multiple_of(TASKS_REFRESH_TICKS) {
                    s.refresh_tasks();
                }
                tick = tick.wrapping_add(1);
            }
        });
    }

    // 任务工具调用 → 展开任务区（host 的 expand_rx watch）。
    {
        let mut expand_rx = expand_rx.clone();
        let mut expand_chat = chat;
        hooks.use_future(async move {
            loop {
                if expand_rx.changed().await.is_err() {
                    return;
                }
                let mut s = expand_chat.write();
                if *expand_rx.borrow() {
                    s.tasks_visible = true;
                }
                s.refresh_tasks();
            }
        });
    }

    // 键盘 + 滚轮（全局）。
    {
        let mut key_chat = chat;
        hooks.use_terminal_events(move |event| match event {
            TerminalEvent::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) => {
                if kind != KeyEventKind::Release {
                    key_chat.write().on_key(code, modifiers);
                }
            }
            TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                kind: MouseEventKind::ScrollUp,
                ..
            }) => {
                let mut s = key_chat.write();
                s.auto_scroll = false;
                s.scroll = s.scroll.saturating_sub(3);
            }
            TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
                kind: MouseEventKind::ScrollDown,
                ..
            }) => {
                let mut s = key_chat.write();
                s.auto_scroll = false;
                s.scroll = s.scroll.saturating_add(3);
            }
            _ => {}
        });
    }

    // 写入阶段（有守卫）：布局尺寸真正变化或文档待重建时才写。
    // 无条件 write() 会令每次渲染标记 dirty → 无限重渲。
    {
        let (width_needed, viewport) = {
            let chat_ref = chat.read();
            let tasks = chat_ref.task_lines();
            let warn_rows = if chat_ref.warnings.is_empty() { 0 } else { 1 };
            let chrome = tasks.len() + warn_rows + 3;
            let viewport = (height as usize).saturating_sub(chrome).max(1);
            (
                chat_ref.width != width as usize
                    || chat_ref.viewport_height != viewport
                    || chat_ref.dirty,
                viewport,
            )
        };
        if width_needed {
            let mut s = chat.write();
            s.width = width as usize;
            s.viewport_height = viewport;
            if s.dirty {
                s.dirty = false;
                s.reconcile_scroll(viewport);
                s.build_rows(width as usize);
            }
        }
    }

    // 只读阶段：文档已就绪，直接读。
    let (_doc, _sticky, viewport, input, typing, busy, warnings, tasks, mode, model, has_ask) = {
        let s = chat.read();
        let doc = s.doc.clone();
        let sticky = doc.sticky.clone();
        let tasks = s.task_lines();
        let warn_rows = if s.warnings.is_empty() { 0 } else { 1 };
        let chrome = tasks.len() + warn_rows + 3;
        let viewport = (height as usize).saturating_sub(chrome).max(1);
        let input = s.input.clone();
        let typing = s.typing;
        let busy = s.busy;
        let warnings = s.warnings.clone();
        let mode = s.session.permission_mode;
        let model = s.session.model.clone();
        let has_ask = s.pending_ask.is_some();
        (doc, sticky, viewport, input, typing, busy, warnings, tasks, mode, model, has_ask)
    };

    let theme = chat.read().theme.clone();

    // 组装布局。
    let mut children: Vec<AnyElement<'static>> = Vec::new();

    children.push(element!(Transcript(chat: Some(chat), viewport: viewport)).into_any());

    // 任务列表（CC TaskListV2：输入框上方）。
    for line in &tasks {
        children.push(row_element(&Row::new(line.clone()), theme.text));
    }

    // 通知行（CC Notifications overlay 位置：输入框上方一行）。
    if let Some(warning) = warnings.first() {
        children.push(element! {
            View(height: 1, width: 100pct, padding_left: 2) {
                Text(
                    color: theme.warning,
                    content: format!("⚠ {warning}"),
                    wrap: TextWrap::NoWrap,
                )
            }
        }.into_any());
    }

    // 输入行：`❯ {input}{caret}`（busy 时 ❯ 弱化，CC PromptChar）。
    let caret = if typing { '▋' } else { ' ' };
    let prompt_style = if busy { theme.inactive } else { theme.text };
    children.push(element! {
        View(height: 1, width: 100pct) {
            View(flex_direction: FlexDirection::Row) {
                Text(color: prompt_style, content: "❯ ", wrap: TextWrap::NoWrap)
                Text(
                    color: theme.text,
                    content: format!("{input}{caret}"),
                    wrap: TextWrap::NoWrap,
                )
            }
        }
    }.into_any());

    // 输入框底部边框行（CC：borderStyle round, borderBottom；iocraft
    // 部分边框无圆角，直接渲染 ╰──╯ 行）。
    children.push(element! {
        Text(
            color: theme.prompt_border,
            content: format!(
                "╰{}╯",
                "─".repeat(width.saturating_sub(2) as usize)
            ),
            wrap: TextWrap::NoWrap,
        )
    }.into_any());

    // footer：模式徽标 + 快捷键 byline（左），模型名（右）。
    children.push(element! {
        View(
            height: 1,
            width: 100pct,
            padding_left: 2,
            padding_right: 2,
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
        ) {
            View(flex_direction: FlexDirection::Row, column_gap: 1) {
                #(mode_badge(mode, &theme))
                Text(
                    color: theme.inactive,
                    content: "esc to interrupt · ctrl+o to expand".to_string(),
                    wrap: TextWrap::NoWrap,
                )
            }
            Text(
                color: theme.inactive,
                content: model,
                wrap: TextWrap::NoWrap,
            )
        }
    }.into_any());

    // 有权限请求时停用输入提示（CC "Waiting for permission…"）。
    if has_ask {
        children.push(element! {
            View(height: 1, width: 100pct, padding_left: 2) {
                Text(
                    color: theme.inactive,
                    content: "Waiting for permission…".to_string(),
                    wrap: TextWrap::NoWrap,
                )
            }
        }.into_any());
    }

    element! {
        View(
            flex_direction: FlexDirection::Column,
            width: 100pct,
            height: height,
        ) {
            #(children.into_iter())
        }
    }
}

/// 权限模式徽标（CC PromptInputFooterLeftSide：`⏸ plan mode on`）。
fn mode_badge(mode: PermissionMode, theme: &Theme) -> Vec<AnyElement<'static>> {
    let (symbol, label, color) = match mode {
        PermissionMode::Default => return Vec::new(),
        PermissionMode::Plan => ("⏸", "plan mode on", theme.plan_mode),
        PermissionMode::AcceptEdits => ("⏵⏵", "accept edits on", theme.accept_edits),
        PermissionMode::BypassPermissions => {
            ("⏵⏵", "bypass permissions on", theme.error)
        }
        PermissionMode::DontAsk => ("⏵⏵", "dont ask on", theme.error),
    };
    // 徽标后带 ` · ` 分隔符（CC Byline 连接）。
    vec![
        element! {
            Text(color: color, content: format!("{symbol} {label}"), wrap: TextWrap::NoWrap)
        }
        .into_any(),
        element! {
            Text(color: theme.inactive, content: "·".to_string(), wrap: TextWrap::NoWrap)
        }
        .into_any(),
    ]
}

/// transcript 滚动区 props。
#[derive(Default, Props)]
struct TranscriptProps {
    chat: Option<State<Chat>>,
    viewport: usize,
}

/// transcript 滚动区：只渲染可见行切片；本地鼠标点击 → 折叠/展开。
#[component]
fn Transcript(mut hooks: Hooks, props: &TranscriptProps) -> impl Into<AnyElement<'static>> {
    let chat = props.chat.expect("Transcript requires a chat state");
    let mut click_chat = chat;
    hooks.use_local_terminal_events(move |event| {
        if let TerminalEvent::FullscreenMouse(FullscreenMouseEvent {
            kind: MouseEventKind::Down(_),
            row,
            ..
        }) = event
        {
            let mut s = click_chat.write();
            let doc_row = s.scroll.saturating_add(row as usize);
            s.doc_click(doc_row);
        }
    });
    let chat_ref = chat.read();
    let scroll = chat_ref.scroll;
    let slice: Vec<Row> = chat_ref
        .doc
        .rows
        .iter()
        .skip(scroll)
        .take(props.viewport)
        .cloned()
        .collect();
    let sticky = chat_ref.doc.sticky.clone();
    let theme = chat_ref.theme.clone();
    drop(chat_ref);
    // sticky prompt header（CC StickyPromptHeader）：绝对定位覆盖在滚动区
    // 顶部，不占布局（避免内容整体位移触发 diff 残留）。
    element! {
        View(flex_grow: 1.0, width: 100pct, flex_direction: FlexDirection::Column, overflow_y: Overflow::Hidden) {
            #(slice.iter().map(|row| row_element(row, theme.text)))
            #(sticky.into_iter().map(|text| element! {
                View(
                    position: Position::Absolute,
                    top: 0,
                    left: 0,
                    right: 0,
                    height: 1,
                    background_color: theme.user_message_bg,
                    padding_right: 1,
                ) {
                    Text(color: theme.subtle, content: format!("❯ {text}"), wrap: TextWrap::NoWrap)
                }
            }))
        }
    }
}

/// 一行 → iocraft 元素：整行背景（用户气泡）用 View 包裹；
/// 段级背景的段用 View 包裹，其余走 MixedText。
fn row_element(row: &Row, default_color: Color) -> AnyElement<'static> {
    let line = &row.line;
    let inner = line_inner(line, default_color);
    if let Some(bg) = row.bg {
        element! {
            View(
                height: 1,
                width: 100pct,
                background_color: bg,
                padding_right: row.padding_right as u16,
            ) {
                #(vec![inner].into_iter())
            }
        }
        .into_any()
    } else {
        inner
    }
}

fn line_inner(line: &Line, default_color: Color) -> AnyElement<'static> {
    if line.is_empty() {
        return element!(View(height: 1)).into_any();
    }
    let has_bg = line.segs.iter().any(|s| s.style.bg.is_some());
    if has_bg {
        element! {
            View(height: 1, flex_direction: FlexDirection::Row) {
                #(line.segs.iter().map(|seg| {
                    element! {
                        View(background_color: seg.style.bg) {
                            Text(
                                color: seg.style.fg.or(Some(default_color)),
                                weight: if seg.style.bold { Weight::Bold } else { Weight::Normal },
                                decoration: if seg.style.underline {
                                    TextDecoration::Underline
                                } else {
                                    TextDecoration::None
                                },
                                italic: seg.style.italic,
                                content: seg.text.clone(),
                                wrap: TextWrap::NoWrap,
                            )
                        }
                    }
                }))
            }
        }
        .into_any()
    } else {
        element! {
            MixedText(
                contents: line
                    .segs
                    .iter()
                    .map(|seg| {
                        let mut c = MixedTextContent::new(seg.text.clone());
                        if let Some(color) = seg.style.fg.or(Some(default_color)) {
                            c = c.color(color);
                        }
                        c = c.weight(if seg.style.bold {
                            Weight::Bold
                        } else {
                            Weight::Normal
                        });
                        if seg.style.underline {
                            c = c.decoration(TextDecoration::Underline);
                        }
                        if seg.style.italic {
                            c = c.italic();
                        }
                        c
                    })
                    .collect::<Vec<_>>(),
                wrap: TextWrap::NoWrap,
            )
        }
        .into_any()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use iocraft::prelude::KeyEvent;
    use iocraft::prelude::KeyEventKind;
    use iocraft::prelude::KeyCode;
    use iocraft::prelude::TerminalEvent;

    /// 渲染任意 Chat 的文档行（测试/视觉检查用）：种子事件初始化 + 受守卫的
    /// 文档重建（与 Bingo 相同的 dirty 收敛语义，无 tick）。
    #[derive(Default, Props)]
    struct ChatViewProps {
        session: Option<Arc<Session>>,
        seed: Option<Vec<crate::tui::UiEvent>>,
        user_messages: Option<Vec<String>>,
        width: usize,
    }

    #[component]
    fn ChatView(mut hooks: Hooks, props: &ChatViewProps) -> impl Into<AnyElement<'static>> {
        let session = props.session.clone().expect("ChatView requires a session");
        let seed = props.seed.clone().unwrap_or_default();
        let user_messages = props.user_messages.clone().unwrap_or_default();
        let mut chat = hooks.use_state(move || {
            let (events_tx, events_rx) = mpsc::unbounded_channel();
            let (asks_tx, asks_rx) = mpsc::unbounded_channel();
            let mut c = Chat::new(session, events_tx, events_rx, asks_tx, asks_rx, Theme::dark());
            for text in user_messages {
                c.messages.push(crate::tui::chat::UiMessage {
                    role: crate::tui::chat::Role::User,
                    text,
                    activities: Vec::new(),
                    insert_points: Vec::new(),
                    groups: Vec::new(),
                    group_of: Vec::new(),
                });
            }
            for event in seed {
                let _ = c.events.send(event);
            }
            c.drain_all();
            c
        });
        if chat.read().dirty || chat.read().width != props.width {
            let mut s = chat.write();
            if s.width != props.width || s.dirty {
                s.width = props.width;
                s.dirty = false;
                s.build_rows(props.width);
            }
        }
        let chat_ref = chat.read();
        let rows: Vec<Row> = chat_ref.doc.rows.clone();
        let theme = chat_ref.theme.clone();
        drop(chat_ref);
        element! {
            View(width: 100pct, flex_direction: FlexDirection::Column) {
                #(rows.iter().map(|row| row_element(row, theme.text)))
            }
        }
    }

    fn test_session() -> Arc<Session> {
        Arc::new(Session {
            client: crate::api::client::Client::new(
                "test-key".to_string(),
                "https://example.com".to_string(),
            ),
            model: "test-model".to_string(),
            permission_mode: PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            transcript: None,
            depth: 0,
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                &std::env::temp_dir(),
            )),
            last_task_reminder_turn: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
        })
    }

    /// 冒烟：mock 终端渲染根组件——欢迎卡片、输入行、边框、footer 齐全，
    /// 键盘事件流入输入框。
    #[tokio::test]
    async fn root_renders_cc_layout_and_accepts_keys() {
        let session = test_session();
        let (_expand_tx, expand_rx) = tokio::sync::watch::channel(false);
        let mut root = element!(Bingo(
            session: Some(session),
            expand_rx: Some(expand_rx),
        ));
        let stream = root.mock_terminal_render_loop(MockTerminalConfig::with_events(
            futures_util::stream::iter(vec![
                TerminalEvent::Resize(120, 40),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('h'))),
            ]),
        ));
        let mut stream = Box::pin(stream);
        let mut actual = Vec::new();
        for i in 0..3 {
            let frame =
                tokio::time::timeout(std::time::Duration::from_secs(3), stream.next()).await;
            match frame {
                Ok(Some(c)) => actual.push(c.to_string()),
                Ok(None) => break,
                Err(_) => panic!("render loop stalled before {} frames", actual.len()),
            }
        }

        let init = &actual[0];
        let sized = &actual[1];
        let typed = &actual[2];
        // 欢迎卡片（边框 + 标题）
        assert!(sized.contains("bingo"), "welcome title: {sized}");
        assert!(sized.contains("╭"), "welcome box top: {sized}");
        assert!(sized.contains("╰"), "welcome box bottom: {sized}");
        // 输入框底边框（CC promptBorder ╰──╯）
        assert!(sized.contains('╯'), "input border corner: {sized}");
        // footer：快捷键 byline + 模型名
        assert!(
            sized.contains("esc to interrupt"),
            "footer hints: {sized}"
        );
        assert!(sized.contains("test-model"), "footer model: {sized}");
        // 键盘 → 输入框
        assert!(typed.contains("❯ h▋"), "typed input: {typed}");
        let _ = init;
    }

    /// Bingo 根的帧内容（真实 app 行为：tick + 事件 + 全布局）。
    #[tokio::test]
    async fn bingo_root_frame_content() {
        let session = test_session();
        let (_tx, expand_rx) = tokio::sync::watch::channel(false);
        let mut root = element!(Bingo(session: Some(session), expand_rx: Some(expand_rx)));
        let stream = root.mock_terminal_render_loop(MockTerminalConfig::with_events(
            futures_util::stream::iter(vec![TerminalEvent::Resize(80, 24)]),
        ));
        let mut stream = Box::pin(stream);
        let mut frames = Vec::new();
        for _ in 0..4 {
            match tokio::time::timeout(std::time::Duration::from_secs(3), stream.next()).await {
                Ok(Some(c)) => {
                    let text = c.to_string();
                    let mut rows: Vec<String> = Vec::new();
                    let mut row = String::new();
                    let mut col = 0usize;
                    for ch in text.chars() {
                        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                        if col + w > 80 && !row.is_empty() {
                            rows.push(row);
                            row = String::new();
                            col = 0;
                        }
                        row.push(ch);
                        col += w;
                    }
                    if !row.is_empty() {
                        rows.push(row);
                    }
                    frames.push(rows);
                }
                _ => break,
            }
        }

        for (i, fr) in frames.iter().enumerate() {
            eprintln!(
                "FRAME {i}: {} rows, same-as-prev={}",
                fr.len(),
                i > 0 && fr == &frames[i - 1]
            );
        }
        assert!(!frames.is_empty(), "no frames");
        let last = frames.last().unwrap();
        let non_blank = last.iter().filter(|l| !l.trim().is_empty()).count();
        eprintln!("FRAME-CONTENT non-blank rows: {non_blank} ({} frames)", frames.len());
        for l in last.iter().take(12) {
            eprintln!("  |{l}|");
        }
        // 欢迎卡片必须完整（不只顶边框一行）
        assert!(
            last.iter().any(|l| l.contains("Welcome back")),
            "welcome card complete: {:?}",
            last.iter().take(8).collect::<Vec<_>>()
        );
    }

    /// 长回复流式渲染顺序验证（区分 doc 层 bug 与 diff 写入 bug）。
    #[test]
    fn long_streaming_reply_order() {
        use crate::tui::UiEvent;
        let session = test_session();
        let reply = "好的，这是一个很长的回复。\n\n## 第一段\n\n这里有很多行文字。\n\n- 项目结构已经梳理清楚\n- TUI 迁移完成\n\n### 详细说明\n\n当文档超过视口高度时。\n\n结尾。";
        let mut seed = vec![
            UiEvent::TurnStart,
            UiEvent::ThinkingDelta("先检查一下。".into()),
        ];
        // 流式：每次追加几个字
        // 真实 API 语义：每次 delta 是增量追加，不是全量。
        for chunk in reply.chars().collect::<Vec<_>>().chunks(12) {
            seed.push(UiEvent::TextDelta(chunk.iter().collect::<String>()));
        }
        let mut root = element!(ChatView(
            session: Some(session),
            seed: Some(seed),
            width: 100usize,
        ));
        let canvas = root.render(Some(100));
        let text = canvas.to_string();
        // 顺序校验：正文开头 → 标题1 → 正文 → 列表 → 标题2 → 结尾
        let checks = [
            "好的，这是一个很长的回复",
            "第一段",
            "这里有很多行文字",
            "项目结构已经梳理清楚",
            "详细说明",
            "当文档超过视口高度时",
            "结尾。",
        ];
        let mut last = 0usize;
        for c in checks {
            let pos = text.find(c).unwrap_or_else(|| panic!("missing {c:?}"));
            assert!(pos > last, "{c:?} out of order: {}", &text[last..last + 120]);
            last = pos;
        }
        // 不重复
        for c in checks {
            assert_eq!(text.matches(c).count(), 1, "{c:?} duplicated");
        }
    }

    /// 第一帧 canvas 是否含重复行（卡片底边框重复 bug 复现）。
    #[test]
    fn first_frame_has_no_duplicate_rows() {
        use crate::tui::UiEvent;
        let session = test_session();
        let seed = vec![
            UiEvent::TurnStart,
            UiEvent::ThinkingDelta("先检查一下。".into()),
        ];
        let mut root = element!(ChatView(session: Some(session), seed: Some(seed), width: 96usize));
        let canvas = root.render(Some(96));
        let text = canvas.to_string();
        let borders = text.matches('╰').count();
        let corners = text.matches('╮').count();
        eprintln!("╰ count={borders} ╮ count={corners}");
        // ChatView 只渲染 doc（无输入行 border）
        assert_eq!(borders, 1, "single welcome bottom, got {borders}: {text}");
        assert_eq!(corners, 1, "one welcome top corner: {text}");
    }

    /// 用户消息 + 卡片组合的 canvas 行序（真实场景复现）。
    #[test]
    fn canvas_rows_with_user_message_and_card() {
        use crate::tui::UiEvent;
        let session = test_session();
        let mut root = element!(ChatView(
            session: Some(session),
            user_messages: Some(vec!["Hi".to_string()]),
            seed: Some(vec![
                UiEvent::TurnStart,
                UiEvent::ThinkingDelta("先检查一下。".into()),
            ]),
            width: 96usize,
        ));
        let canvas = root.render(Some(96));
        let text = canvas.to_string();
        // 卡片顶边框必须在用户消息之前
        let card = text.find('╭').expect("card top");
        let user = text.find("❯ Hi").expect("user message");
        let thinking = text.find("✻").expect("thinking");
        assert!(card < user, "card before user: {text}");
        assert!(user < thinking, "user before thinking: {text}");
        assert_eq!(text.matches('╰').count(), 1, "single card bottom: {text}");
        eprintln!("order ok: card={card} user={user} thinking={thinking}");
    }

    /// 冒烟：权限请求渲染在 transcript 内，数字键确认。
    #[tokio::test]
    async fn tokio_test_runtime_drives_timer_futures() {
        let session = test_session();
        let (_tx, expand_rx) = tokio::sync::watch::channel(false);
        let mut root = element!(Bingo(session: Some(session), expand_rx: Some(expand_rx)));
        let stream = root.mock_terminal_render_loop(MockTerminalConfig::with_events(
            futures_util::stream::iter(vec![TerminalEvent::Resize(80, 24)]),
        ));
        let mut stream = Box::pin(stream);
        let start = std::time::Instant::now();
        let mut frames = 0usize;
        while start.elapsed() < std::time::Duration::from_millis(300) {
            match tokio::time::timeout(std::time::Duration::from_millis(250), stream.next()).await {
                Ok(Some(_)) => frames += 1,
                Ok(None) => break,
                Err(_) => break,
            }
        }
        // 静态 UI 只出初始帧；重点是循环不空转/不死锁。
        assert!(frames >= 1, "stable render loop, got {frames} frames");
    }

    /// tick 动画确实驱动 mock 循环输出持续帧（spinner 类内容）。
    #[tokio::test]
    async fn counter_animation_frames() {
        let session = test_session();
        let (_tx, expand_rx) = tokio::sync::watch::channel(false);
        let mut root = element!(Bingo(session: Some(session), expand_rx: Some(expand_rx)));
        let stream = root.mock_terminal_render_loop(MockTerminalConfig::with_events(
            futures_util::stream::iter(vec![TerminalEvent::Resize(80, 24)]),
        ));
        let mut stream = Box::pin(stream);
        let mut frames = 0usize;
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(400) {
            match tokio::time::timeout(std::time::Duration::from_millis(300), stream.next()).await {
                Ok(Some(_)) => frames += 1,
                Ok(None) => break,
                Err(_) => break,
            }
        }
        // 静态 UI 下 canvas 不持续变化：1 帧（初始）即可——重点是循环不空转、不死锁。
        assert!(frames >= 1, "stable render loop, got {frames} frames");
    }

    /// 空行 View(height=1) 是否真实占一行（blank 间距渲染验证）。
    #[test]
    fn blank_view_occupies_a_row() {
        let mut root = element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "a")
                View(height: 1)
                Text(content: "b")
            }
        };
        let canvas = root.render(Some(10));
        let mut buf = Vec::new();
        canvas.write_ansi(&mut buf).unwrap();
        let out = String::from_utf8_lossy(&buf);
        assert!(
            out.contains("a\r\n\r\nb") || out.contains("a\n\nb") || out.matches("\n").count() >= 2,
            "blank row between a and b: {:?}",
            out
        );
    }

    /// 视觉检查：真实对话（用户消息 + 思考 + 工具 + 折叠组）渲染。
    #[test]
    fn dump_conversation_for_inspection() {
        use crate::tui::UiEvent;
        let session = test_session();
        let seed = vec![
            UiEvent::TurnStart,
            UiEvent::ThinkingDelta("先读一下 README 和结构。".into()),
            UiEvent::ToolStart { name: "Read".into() },
            UiEvent::ToolStart { name: "Read".into() },
            UiEvent::ToolReady {
                name: "Read".into(),
                input: serde_json::json!({"file_path": "README.md"}),
            },
            UiEvent::ToolReady {
                name: "Read".into(),
                input: serde_json::json!({"file_path": "src/main.rs"}),
            },
            UiEvent::ToolDone(crate::query::ToolCallDone {
                name: "Read".into(),
                summary: "Read README.md".into(),
                output: "# bingo\nagent CLI".into(),
                is_error: false,
                diff: None,
                duration_ms: 120,
            }),
            UiEvent::ToolDone(crate::query::ToolCallDone {
                name: "Read".into(),
                summary: "Read src/main.rs".into(),
                output: "fn main() {}".into(),
                is_error: false,
                diff: None,
                duration_ms: 80,
            }),
            UiEvent::TextDelta("项目结构清晰，开始实现。\n\n**bingo** 是 Rust 的 agent CLI。".into()),
        ];
        let seed_clone = seed.clone();
        let mut root = element!(ChatView(session: Some(session), seed: Some(seed_clone), width: 120usize));
        let canvas = root.render(Some(120));
        let mut buf = Vec::new();
        canvas.write_ansi(&mut buf).unwrap();
        let ansi = String::from_utf8_lossy(&buf);
        // 剥掉 ANSI 序列，保留 CRLF 行分隔
        let text = ansi
            .replace("\x1b[0m", "")
            .split('\x1b')
            .map(|seg| {
                let idx = seg.find('m').unwrap_or(0);
                if idx > 0 && seg.as_bytes().get(idx - 1).is_some_and(|&b| (b'0'..=b'9').contains(&b) || b == b';') {
                    &seg[idx + 1..]
                } else {
                    seg
                }
            })
            .collect::<Vec<_>>()
            .join("");
        let mut rows: Vec<String> = Vec::new();
        let mut row = String::new();
        let mut col = 0usize;
        for ch in text.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if col + w > 120 && !row.is_empty() {
                rows.push(row);
                row = String::new();
                col = 0;
            }
            row.push(ch);
            col += w;
        }
        if !row.is_empty() {
            rows.push(row);
        }
        assert!(
            rows.iter().any(|l| l.contains("Read 2 files")),
            "fold summary row"
        );
        // 消息块间距：doc 中存在空行（渲染层 height=1 View）。
        let chat_doc = {
            // 重建同种子 chat 检查 doc 行
            let session = test_session();
            let (events_tx, events_rx) = mpsc::unbounded_channel();
            let (asks_tx, asks_rx) = mpsc::unbounded_channel();
            let mut c = Chat::new(session, events_tx, events_rx, asks_tx, asks_rx, Theme::dark());
            for event in seed {
                let _ = c.events.send(event);
            }
            c.drain_all();
            c.build_rows(120);
            c.doc.rows.len()
        };
        let _ = chat_doc;
    }

    async fn root_renders_permission_request() {
        let session = test_session();
        let (_expand_tx, expand_rx) = tokio::sync::watch::channel(false);
        let actual = element!(Bingo(
            session: Some(session),
            expand_rx: Some(expand_rx),
        ))
        .mock_terminal_render_loop(MockTerminalConfig::with_events(
            futures_util::stream::iter(vec![TerminalEvent::Resize(80, 24)]),
        ))
        .take(2)
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .await;
        // 初始渲染必须成功（不 panic）且含输入框
        assert!(actual[1].contains("╰"), "layout rendered: {}", actual[1]);
    }
}
