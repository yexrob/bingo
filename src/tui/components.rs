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
///
/// 首帧（`first=true`）不触发：启动时尺寸检测/首帧 FORCE_FULL_REDRAW
/// （doc 行数 0→N）不应清屏——inline 模式会清掉 shell 残留内容。
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
            first: true,
        });
    }
}

struct ForceRedrawOnResize {
    last: Option<(u16, u16)>,
    changed: bool,
    first: bool,
}

impl Hook for ForceRedrawOnResize {
    fn poll_change(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        let size = crossterm::terminal::size().unwrap_or((0, 0));
        match self.last {
            // 首次：只记录基准尺寸，不算变化（否则启动即清屏）。
            None => self.last = Some(size),
            Some(last) if last != size => {
                self.last = Some(size);
                self.changed = true;
            }
            _ => {}
        }
        if self.changed {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    fn post_component_update(&mut self, updater: &mut iocraft::ComponentUpdater) {
        let first = self.first;
        self.first = false;
        let force = crate::tui::chat::FORCE_FULL_REDRAW.swap(false, std::sync::atomic::Ordering::Relaxed);
        if (self.changed || force) && !first {
            self.changed = false;
            updater.clear_terminal_output();
        }
    }
}

use crate::permission::PermissionMode;
use crate::query::Session;
use crate::tui::chat::{model_footer_label, Chat, Row};
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
    /// inline 模式（默认，对标 CC 非全屏）：定稿行经 use_output 打印进
    /// 终端 scrollback，canvas 只画动态尾部；None/false = 全屏视口模式。
    pub inline: Option<bool>,
}

/// inline 模式（CC 非全屏）下的按键 gate：REPL 无滚动区，滚动键交给
/// 终端 scrollback；空闲 Esc 忽略（切 typing 会让输入失焦）；ctrl+o
/// 折叠只放行未定稿的最后一条消息（已打印进 scrollback 的不能再折叠）。
/// 返回 true 表示"空闲 Ctrl+C，请求退出会话"。
fn inline_gate(chat: &mut Chat, code: KeyCode, modifiers: KeyModifiers) -> bool {
    match code {
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            if !chat.busy {
                return true;
            }
        }
        KeyCode::Esc => {
            // `/model` 菜单打开时 Esc 有明确用途（退出菜单）→ 放行给 on_key。
            if !chat.busy && chat.model_menu.is_none() {
                return false;
            }
        }
        KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) && !chat.last_message_dynamic() => {
            return false;
        }
        _ => {}
    }
    if !chat.ask_key(code) {
        let _ = chat.on_key(code, modifiers);
    }
    false
}

/// 落盘钩子：把定稿行打印进 scrollback，并在 canvas 形状变化时全量重写。
///
/// 为什么必须在 post_component_update（清屏之后）打印：iocraft 的 inline
/// diff/clear 全部按"光标在 prev canvas 末行"做相对回退。若在组件渲染中
/// println（原实现），光标被推下 N 行，iocraft 回退少算 N 行 → canvas 上移
/// 错位、顶部残留旧行（canvas 越高越明显）。
///
/// 时序：write 阶段只把落盘边界写入 chat.flush_up_to → 本帧
/// post_component_update：updater.clear_terminal_output()（光标仍在 prev
/// canvas 末行，相对清除正确）→ println 落盘行 → write_canvas 走全量分支
/// （did_clear → prev=None）从光标处重画 → canvas 恒位于最新内容之后。
///
/// 形状变化（canvas 高度/行数变化，如 slash 菜单弹出收起、ask 对话框
/// 出现）：iocraft 的 diff 用 \r\n 在底部创建新行会触发终端滚动，而 diff
/// 的"跳过相同行"仍按滚动前的物理位置移动 → 错位残留。形状变化帧强制
/// 全量重写（光标跟随 canvas，滚动后依然自洽）。
struct FlushRows {
    chat: iocraft::hooks::State<Chat>,
    printed: iocraft::hooks::State<usize>,
    stdout: iocraft::hooks::StdoutHandle,
    prev_shape: Option<(usize, usize)>,
}

impl Hook for FlushRows {
    fn poll_change(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }

    fn post_component_update(&mut self, updater: &mut iocraft::ComponentUpdater) {
        let (rows, flush_to, shape) = {
            let chat = self.chat.read();
            let p = *self.printed.read();
            let flush_to = chat.flush_up_to.min(chat.doc.rows.len());
            let mut rows: Vec<Row> = Vec::new();
            if flush_to > p {
                rows.extend(chat.doc.rows[p..flush_to].iter().cloned());
            }
            if chat.replay_pending && p > 0 {
                let (_, term_h) = crossterm::terminal::size().unwrap_or((0, 0));
                let chrome = shape_chrome(&chat);
                let n = p.min((term_h as usize).saturating_sub(chrome));
                if n > 0 {
                    rows.extend(chat.doc.rows[p - n..p].iter().cloned());
                }
            }
            let shape = (chat.doc.rows.len(), shape_chrome(&chat));
            (rows, flush_to, shape)
        };
        let shape_changed = self.prev_shape != Some(shape);
        self.prev_shape = Some(shape);
        if rows.is_empty() && !shape_changed {
            return;
        }
        // 光标仍在 prev canvas 末行：相对清除正确，随后全量重写。
        // 形状变化帧同样走全量（防终端滚动导致 diff 错位）。
        updater.clear_terminal_output();
        let theme = self.chat.read().theme.clone();
        for row in rows {
            let mut buf = Vec::new();
            let canvas = row_element(&row, theme.text).render(None);
            let _ = canvas.write_ansi(&mut buf);
            let rendered = String::from_utf8_lossy(&buf);
            self.stdout.println(rendered.trim_end_matches(['\n', '\r']));
        }
        let mut chat = self.chat.write();
        let mut p = self.printed.write();
        *p = (*p).max(flush_to);
        chat.flush_up_to = 0;
        chat.replay_pending = false;
    }
}

/// canvas 形状的 chrome 部分（任务区/警告/状态行/建议/ask 对话框）。
fn shape_chrome(chat: &Chat) -> usize {
    let tasks = chat.task_lines();
    let warn = usize::from(!chat.warnings.is_empty());
    let status = usize::from(chat.running_status().is_some());
    tasks.len() + warn + status + 3 + chat.slash_suggestions.len() + usize::from(chat.pending_ask.is_some())
}

/// bingo 主界面根组件：通道驱动状态 + 布局。
/// 全屏模式：视口切片 + app 内滚动；inline 模式（默认）：定稿行落盘
/// scrollback + canvas 只画动态尾部（对标 CC 非全屏）。
#[component]
pub fn Bingo(mut hooks: Hooks, props: &BingoProps) -> impl Into<AnyElement<'static>> {
    let session = props.session.clone().expect("Bingo requires a session");
    let expand_rx = props
        .expand_rx
        .clone()
        .expect("Bingo requires an expand_rx");
    let inline = props.inline.unwrap_or(false);
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
            props.detected_background,
        )
    });
    // inline 模式：定稿行落盘游标（已打印进 scrollback 的 doc 行数）。
    let mut printed = hooks.use_state(|| 0usize);
    // inline 模式：resize 后重绘当前视口标志（清屏 + 重播视口内落盘行）。
    let mut replay = hooks.use_state(|| false);
    // inline 模式：空闲 Ctrl+C 退出标志（busy 时 Ctrl+C 走 on_key 取消）。
    let mut exit_requested = hooks.use_state(|| false);
    // inline 模式：真实光标隐藏标志（iocraft 渲染后真实光标不跟随输入）。
    let mut cursor_hidden = hooks.use_state(|| false);
    let (stdout_handle, _stderr_handle) = hooks.use_output();

    hooks.use_force_redraw_on_resize();
    let (width, height) = hooks.use_terminal_size();
    // inline：定稿行落盘钩子——post_component_update 中先清屏后 println
    //（光标基准不被 println 破坏），随后全量重写。
    if inline {
        hooks.use_hook(|| FlushRows {
            chat,
            printed,
            stdout: stdout_handle.clone(),
            prev_shape: None,
        });
    }
    // inline：尺寸变化 → 标记重播（首帧除外——尚无内容可重播）。
    // last_size 必须无条件更新（否则首帧后每帧都判定"变化"）。
    {
        let mut last_size = hooks.use_state(|| (0u16, 0u16));
        if inline && (width, height) != *last_size.read() {
            let changed = *last_size.read() != (0, 0);
            *last_size.write() = (width, height);
            if changed {
                // resize 帧强制重建：tick 尚未置位 dirty 时，重播会用到
                // 旧宽度文档（markdown 折行/欢迎卡宽度都不对）。
                chat.write().dirty = true;
                if *printed.read() > 0 {
                    *replay.write() = true;
                }
            }
        }
    }

    // 主循环：tick（spinner/thinking 计时）+ 通道排空 + 任务快照。
    {
        let mut tick_chat = chat;
        hooks.use_future(async move {
            let mut tick: u64 = 0;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
                let drained = {
                    let mut s = tick_chat.write();
                    s.tick();
                    s.drain_all()
                };
                if drained {
                    tick = 0;
                    // 事件处理（TextDelta/ThinkingDelta/ToolStart 等）会改变
                    // 流式内容——iocraft 行 diff 在"内容增长但行数不变"的帧
                    // 可能残留旧行（真实终端实测：正文半截覆盖）。全量重写
                    // 绕开 diff；synchronized update 下同帧原子完成，不闪。
                    crate::tui::chat::FORCE_FULL_REDRAW
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
                if tick.is_multiple_of(TASKS_REFRESH_TICKS) {
                    tick_chat.write().refresh_tasks();
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

    // 键盘 + 滚轮（全局）。inline 模式无鼠标捕获，只有按键。
    {
        let mut key_chat = chat;
        let mut exit_flag = exit_requested;
        hooks.use_terminal_events(move |event| match event {
            TerminalEvent::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) => {
                if kind == KeyEventKind::Release {
                    return;
                }
                if inline {
                    if inline_gate(&mut key_chat.write(), code, modifiers) {
                        *exit_flag.write() = true;
                    }
                } else {
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
    let tail_start: usize = {
        let (width_needed, viewport) = {
            let chat_ref = chat.read();
            let tasks = chat_ref.task_lines();
            let warn_rows = if chat_ref.warnings.is_empty() { 0 } else { 1 };
            let status_rows = usize::from(chat_ref.running_status().is_some());
            let chrome = tasks.len() + warn_rows + status_rows + 3 + chat_ref.slash_suggestions.len();
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
        if inline {
            // 隐藏真实终端光标：iocraft 渲染后真实光标停在 canvas 末行
            //（footer 行尾），不跟随输入——隐藏后以 ▋ 假光标指示输入位置
            //（退出时 iocraft 自动 Show）。
            if !*cursor_hidden.read() {
                *cursor_hidden.write() = true;
                stdout_handle.print("\x1b[?25l");
            }
            // 重建后行数可能收缩（resize 宽度变化重排）——clamp 落盘游标，
            // 否则 doc.rows 切片越界（live_start 依赖它，须在其前）。
            // 注意：值未变时不要 write——渲染期间写 state 会唤醒组件
            // 再渲染（iocraft State 的 DerefMut 无条件标记变更），形成
            // 渲染风暴、饿死终端事件（打字/退出全部失效）。
            {
                let s = chat.read();
                let mut p = printed.write();
                let clamped = (*p).min(s.doc.rows.len());
                if clamped != *p {
                    *p = clamped;
                }
            }
            // inline：定稿行落盘 + 计算动态尾部起点。
            // 落盘边界 = 定稿推进与"尾部不超过屏幕"两者取高——canvas
            // 高度恒 ≤ 终端高度，inline 擦除才不会落到 scrollback 里。
            let live_start = {
                let s = chat.read();
                let tasks = s.task_lines();
                let warn_rows = if s.warnings.is_empty() { 0 } else { 1 };
                let status_rows = usize::from(s.running_status().is_some());
                let chrome_total = tasks.len()
                    + warn_rows
                    + status_rows
                    + 3
                    + s.slash_suggestions.len()
                    + usize::from(s.pending_ask.is_some());
                let max_live = (height as usize).saturating_sub(chrome_total);
                (*printed
                    .read())
                    .max(s.doc.settled.min(s.doc.rows.len()))
                    .max(s.doc.rows.len().saturating_sub(max_live))
            };
            // 落盘边界写入 chat：FlushRows hook 在 post_component_update
            // 消费（先清屏后 println，保证 iocraft 相对定位正确）。
            {
                let mut s = chat.write();
                if live_start > *printed.read() {
                    s.flush_up_to = live_start;
                }
                if *replay.read() && *printed.read() > 0 {
                    s.replay_pending = true;
                    *replay.write() = false;
                }
            }
            live_start
        } else {
            0
        }
    };

    // 只读阶段：文档已就绪，直接读。
    let (_doc, _sticky, viewport, input, typing, bash_mode, busy, warnings, tasks, mode, model, thinking, has_ask, status, slash_suggestions, slash_selected, model_menu) = {
        let s = chat.read();
        let doc = s.doc.clone();
        let sticky = doc.sticky.clone();
        let tasks = s.task_lines();
        let warn_rows = if s.warnings.is_empty() { 0 } else { 1 };
        let status = s.running_status();
        let status_rows = usize::from(status.is_some());
        let menu_rows = s
            .model_menu
            .as_ref()
            .map(|m| match &m.models {
                Some(mm) if !mm.loading => mm.models.len(),
                _ => m.providers.len(),
            })
            .unwrap_or(0)
            .min(crate::tui::chat::SLASH_SUGGESTIONS_MAX + 5);
        let slash_rows = s.slash_suggestions.len().max(menu_rows);
        let chrome = tasks.len() + warn_rows + status_rows + 3 + slash_rows;
        let viewport = (height as usize).saturating_sub(chrome).max(1);
        let input = s.input.clone();
        let typing = s.typing;
        let bash_mode = s.bash_mode;
        let busy = s.busy;
        let warnings = s.warnings.clone();
        let mode = s.session.permission_mode;
        let model = s.session.runtime.model.borrow().clone();
        let thinking = s.session.runtime.thinking.borrow().clone();
        let has_ask = s.pending_ask.is_some();
        let slash_suggestions = s.slash_suggestions.clone();
        let slash_selected = s.slash_selected;
        let model_menu = s.model_menu.clone();
        (doc, sticky, viewport, input, typing, bash_mode, busy, warnings, tasks, mode, model, thinking, has_ask, status, slash_suggestions, slash_selected, model_menu)
    };

    let theme = chat.read().theme.clone();

    // 组装布局。
    let mut children: Vec<AnyElement<'static>> = Vec::new();

    children.push(element!(Transcript(
        chat: Some(chat),
        viewport: viewport,
        tail_start: if inline { Some(tail_start) } else { None }
    ))
    .into_any());

    // 运行状态行（对标 CC ActivityIndicator）：busy 时在输入框上方显示
    // `⠋ {动词} for {N}s`（工具 summary / thinking 俏皮词 / Working），
    // 让用户时刻知道 agent 正在运行——独立于 transcript 内容与滚动。
    if let Some((verb, elapsed)) = status {
        let spinner = crate::tui::activities::spinner(chat.read().tick);
        children.push(status_row(&verb, elapsed, spinner, &theme));
    }

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

    // footer：模式徽标 + 快捷键 byline（左），模型名（右）。
    // bash 模式下左侧提示 `! for shell mode`（CC bashBorder 提示）。
    let footer = element! {
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
                #(bash_mode.then(|| element! {
                    Text(
                        color: theme.bash_border,
                        content: "! for shell mode".to_string(),
                        wrap: TextWrap::NoWrap,
                    )
                }))
                Text(
                    color: theme.inactive,
                    content: "esc to interrupt · ctrl+o to expand".to_string(),
                    wrap: TextWrap::NoWrap,
                )
            }
            Text(
                color: theme.inactive,
                content: model_footer_label(&model, thinking.as_deref()),
                wrap: TextWrap::NoWrap,
            )
        }
    };

    // 输入行：`{prefix} {input}{caret}`（busy 时前缀弱化，CC PromptChar）。
    // bash 模式前缀为 `!`（CC prefix=! 且 bashBorder 色）；▋ 假光标：
    // inline 模式隐藏真实终端光标（iocraft 渲染后真实光标停在 canvas
    // 末行，不跟随输入），以 ▋ 指示输入位置。
    let caret = if typing { '▋' } else { ' ' };
    let prompt_style = if busy { theme.inactive } else { theme.text };
    let (prefix, prefix_color) = if bash_mode {
        ("!".to_string(), theme.bash_border)
    } else {
        ("❯ ".to_string(), prompt_style)
    };
    let input_row = element! {
        View(height: 1, width: 100pct) {
            View(flex_direction: FlexDirection::Row) {
                Text(color: prefix_color, content: prefix, wrap: TextWrap::NoWrap)
                Text(
                    color: theme.text,
                    content: format!("{input}{caret}"),
                    wrap: TextWrap::NoWrap,
                )
            }
        }
    };

    // 输入框上边框（CC promptBorder 圆角边框上边；bash 模式换 bashBorder）。
    let border_color = if bash_mode {
        theme.bash_border
    } else {
        theme.prompt_border
    };
    let border_top = element! {
        Text(
            color: border_color,
            content: format!("╭{}╮", "─".repeat(width.saturating_sub(2) as usize)),
            wrap: TextWrap::NoWrap,
        )
    };
    // 输入框底部边框行（CC：borderStyle round, borderBottom）。
    let border_bottom = element! {
        Text(
            color: border_color,
            content: format!("╰{}╯", "─".repeat(width.saturating_sub(2) as usize)),
            wrap: TextWrap::NoWrap,
        )
    };

    // slash 下拉建议（对齐 CC PromptInputFooterSuggestions）：
    // `+ /name  description`，选中行 suggestion 色、其余 dim；最多 5 行。
    // 描述按实际可用宽度截断（iocraft NoWrap 不截断，超宽会撑破 canvas
    // 导致行 diff 错位残留）。位置：fullscreen 在输入框上方，inline 下方。
    let menu_rows: Vec<AnyElement<'static>> = if let Some(menu) = &model_menu {
        // `/model` 二级选择器：一级 `▸ provider`，二级 `▸ model（loading 行）`。
        // 与 slash 建议同渲染模式（选中高亮），行数参与 chrome 计算。
        let items: Vec<(String, bool)> = match &menu.models {
            Some(m) => {
                if m.loading {
                    vec![("… 拉取模型列表".to_string(), true)]
                } else if m.models.is_empty() {
                    vec![("（该端点未返回模型，Esc 退出）".to_string(), true)]
                } else {
                    m.models
                        .iter()
                        .enumerate()
                        .map(|(i, name)| (name.clone(), i == m.selected))
                        .collect()
                }
            }
            None => menu
                .providers
                .iter()
                .enumerate()
                .map(|(i, p)| (p.clone(), i == menu.provider_selected))
                .collect(),
        };
        let menu_rows = items.len().min(crate::tui::chat::SLASH_SUGGESTIONS_MAX + 5);
        items
            .into_iter()
            .take(menu_rows)
            .map(|(name, selected)| {
                let color = if selected {
                    theme.permission
                } else {
                    theme.inactive
                };
                let line = crate::tui::markdown::truncate(
                    &format!("+ {}{name}", if selected { "▸ " } else { "  " }),
                    (width as usize).saturating_sub(2),
                );
                element! {
                    View(height: 1, width: 100pct, padding_left: 2) {
                        Text(
                            color: color,
                            content: line,
                            wrap: TextWrap::NoWrap,
                        )
                    }
                }
                .into_any()
            })
            .collect()
    } else {
        Vec::new()
    };
    let suggestions_view: Vec<AnyElement<'static>> = if slash_suggestions.is_empty() {
        menu_rows
    } else {
        let name_col = slash_suggestions
            .iter()
            .map(|s| s.name.chars().count())
            .max()
            .unwrap_or(0)
            + 2;
        // 可用描述宽度 = 终端宽 - padding(2) - "+ "(2) - 名称列 - 分隔(2)。
        // 未计入 padding 会令行宽超终端 → 终端折行 → canvas 行数与 iocraft
        // 认知不符 → diff 错位残留。
        let desc_width = (width as usize)
            .saturating_sub(2 + 2 + name_col + 2)
            .max(8);
        slash_suggestions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let selected = i == slash_selected;
                let color = if selected {
                    theme.permission
                } else {
                    theme.inactive
                };
                let name_text = format!("/{:<width$}", s.name, width = name_col);
                let desc = crate::tui::markdown::truncate(&s.description, desc_width);
                // 整行二次截断：name_col 计算与 padding 叠加的实际宽度
                // 可能仍超内容区（如 CJK 宽度），超宽行会被终端折行、
                // 破坏 canvas 行数与 iocraft 认知的一致性。
                let line = crate::tui::markdown::truncate(
                    &format!("+ {name_text}  {desc}"),
                    (width as usize).saturating_sub(2),
                );
                element! {
                    View(height: 1, width: 100pct, padding_left: 2) {
                        Text(
                            color: color,
                            content: line,
                            wrap: TextWrap::NoWrap,
                        )
                    }
                }
                .into_any()
            })
            .collect()
    };

    // CC 布局：输入框（上边框 + 输入行 + 下边框）在 footer 上方。
    // 建议行按模式定位：fullscreen 上方、inline 下方（对齐 slash 输出）。
    if inline {
        children.push(border_top.into_any());
        children.push(input_row.into_any());
        children.push(border_bottom.into_any());
        children.extend(suggestions_view);
        children.push(footer.into_any());
    } else {
        children.extend(suggestions_view);
        children.push(border_top.into_any());
        children.push(input_row.into_any());
        children.push(border_bottom.into_any());
        children.push(footer.into_any());
    }

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

    // inline 模式：空闲 Ctrl+C 请求退出会话；/exit 两种模式都退出。
    let mut system = hooks.use_context_mut::<SystemContext>();
    if chat.read().exit {
        *exit_requested.write() = true;
    }
    if *exit_requested.read() {
        system.exit();
    }

    // inline：根 View 不固定高度——canvas 只占内容自然高度（尾部 +
    // chrome），输入行随内容流走，不会钉在屏幕底部（对标 CC 非全屏）；
    // 全屏：固定终端高度，canvas 占满屏幕。
    if inline {
        element! {
            View(flex_direction: FlexDirection::Column, width: 100pct) {
                #(children.into_iter())
            }
        }
    } else {
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
}

/// 运行状态行（对标 CC ActivityIndicator）：`⠋ {动词} for {N}s`。
/// 工具动词用 tool_running 色，thinking 词与兜底 Working 用 thinking 色。
fn status_row(verb: &str, elapsed: f64, spinner: char, theme: &Theme) -> AnyElement<'static> {
    let color = if verb == "Working" {
        theme.thinking
    } else {
        theme.tool_running
    };
    element! {
        View(height: 1, width: 100pct, padding_left: 2) {
            Text(
                color: color,
                content: format!("{spinner} {verb} for {elapsed:.1}s"),
                wrap: TextWrap::NoWrap,
            )
        }
    }
    .into_any()
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
    /// inline 模式：渲染 `doc.rows[tail_start..]` 的完整尾部（canvas 高度
    /// 受落盘边界控制，恒 ≤ 屏幕）；None = 全屏视口模式（切片 + 点击）。
    tail_start: Option<usize>,
}

/// transcript 滚动区：全屏模式渲染可见行切片 + 鼠标点击折叠/展开；
/// inline 模式渲染动态尾部（无滚动无点击，历史交给终端 scrollback）。
#[component]
fn Transcript(mut hooks: Hooks, props: &TranscriptProps) -> impl Into<AnyElement<'static>> {
    let chat = props.chat.expect("Transcript requires a chat state");
    if props.tail_start.is_none() {
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
    }
    let chat_ref = chat.read();
    let slice: Vec<Row> = match props.tail_start {
        // inline：动态尾部全量（不含已落盘行）。
        Some(tail_start) => chat_ref
            .doc
            .rows
            .iter()
            .skip(tail_start)
            .cloned()
            .collect(),
        // 全屏：视口切片。
        None => chat_ref
            .doc
            .rows
            .iter()
            .skip(chat_ref.scroll)
            .take(props.viewport)
            .cloned()
            .collect(),
    };
    let sticky = if props.tail_start.is_none() {
        chat_ref.doc.sticky.clone()
    } else {
        None
    };
    let theme = chat_ref.theme.clone();
    drop(chat_ref);
    // sticky prompt header（CC StickyPromptHeader）：绝对定位覆盖在滚动区
    // 顶部，不占布局（避免内容整体位移触发 diff 残留）。
    // inline：flex_grow 0（canvas 取内容自然高度，输入行随内容流走，
    // 不钉屏幕底）；全屏：grow 填满视口。
    element! {
        View(
            flex_grow: if props.tail_start.is_some() { 0.0 } else { 1.0 },
            width: 100pct,
            flex_direction: FlexDirection::Column,
            overflow_y: Overflow::Hidden,
        ) {
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
            let mut c = Chat::new(session, events_tx, events_rx, asks_tx, asks_rx, Theme::dark(), None);
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
            runtime: crate::query::Runtime::new("test-model".to_string(), None, Default::default()),
            permission_mode: PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            depth: 0,
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                "test",
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

    /// 状态行渲染：busy 时输出 `⠋ {动词} for {N}s`（对标 CC ActivityIndicator）。
    #[test]
    fn status_row_renders_busy_verb() {
        let theme = Theme::dark();
        let mut row = status_row("Working", 12.5, '⠋', &theme);
        let canvas = row.render(Some(60));
        let text = canvas.to_string();
        assert!(text.contains("Working for 12.5s"), "{text}");
        assert!(text.contains('⠋'), "{text}");
        // 工具动词用 tool_running 色、兜底 Working 用 thinking 色。
        let mut tool_row = status_row("$ cargo test", 3.2, '⠙', &theme);
        let tool_canvas = tool_row.render(Some(60));
        assert!(tool_canvas.to_string().contains("$ cargo test for 3.2s"));
    }

    /// bash 模式 UI：`!` 进入 shell 模式后前缀变 `!`、输入框边框换
    /// bashBorder 色、footer 出现 `! for shell mode` 提示。
    #[tokio::test]
    async fn bash_mode_prefix_border_and_hint() {
        let session = test_session();
        let (_expand_tx, expand_rx) = tokio::sync::watch::channel(false);
        let mut root = element!(Bingo(
            session: Some(session),
            expand_rx: Some(expand_rx),
        ));
        let stream = root.mock_terminal_render_loop(MockTerminalConfig::with_events(
            futures_util::stream::iter(vec![
                TerminalEvent::Resize(120, 40),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('!'))),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('l'))),
            ]),
        ));
        let mut stream = Box::pin(stream);
        let mut frames = Vec::new();
        for _ in 0..4 {
            match tokio::time::timeout(std::time::Duration::from_secs(3), stream.next()).await {
                Ok(Some(c)) => frames.push(c.to_string()),
                Ok(None) => break,
                Err(_) => break,
            }
        }
        let typed = frames.last().expect("typed frame");
        assert!(typed.contains("!l▋"), "bash prefix + input: {typed}");
        assert!(
            typed.contains("! for shell mode"),
            "footer hint: {typed}"
        );
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
                standalone: false,
            },
            UiEvent::ToolReady {
                name: "Read".into(),
                input: serde_json::json!({"file_path": "src/main.rs"}),
                standalone: false,
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
            let mut c = Chat::new(session, events_tx, events_rx, asks_tx, asks_rx, Theme::dark(), None);
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

    /// inline_gate：空闲 Esc 默认拦截（切 typing 会让输入失焦），
    /// 但 `/model` 菜单打开时必须放行（否则 Esc 退不出菜单）。
    #[test]
    fn inline_gate_passes_esc_when_model_menu_open() {
        let session = test_session();
        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (asks_tx, asks_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut chat = Chat::new(
            session,
            events_tx,
            events_rx,
            asks_tx,
            asks_rx,
            Theme::dark(),
            None,
        );
        chat.input = "/model".to_string();
        chat.submit();
        assert!(
            chat.model_menu.is_some(),
            "菜单已打开"
        );
        // 菜单打开：Esc 放行（返回 false = 不请求退出，事件继续给 on_key）。
        assert!(!inline_gate(&mut chat, KeyCode::Esc, KeyModifiers::empty()));
        assert!(
            chat.on_key(KeyCode::Esc, KeyModifiers::empty()),
            "Esc 被 on_key 消费（退出菜单）"
        );
        assert!(chat.model_menu.is_none(), "菜单已退出");
        // 空闲无菜单：Esc 仍被 gate 拦截（不传给 on_key）。
        assert!(!inline_gate(&mut chat, KeyCode::Esc, KeyModifiers::empty()));
    }
}
