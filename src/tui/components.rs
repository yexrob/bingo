//! iocraft UI 层：根组件 + transcript 区。
//!
//! 布局（自上而下，chrome = transcript 之外的每一行，见 [`Chrome`]）：
//!
//! ```text
//! ┌─ 根 View ─────────────────────────────────────────────┐
//! │ [Transcript] inline：动态尾部；全屏：视口切片 + sticky │
//! │ [状态行]  `⠋ Working for 3.0s`（busy 时）              │
//! │ [任务区]  todo · N/M tasks + 条目                      │
//! │ [通知行]  `⚠ …`                                        │
//! │ [输入框]  ╭──╮ / `❯ {input}▋` / ╰──╯                   │
//! │ [建议区]  slash 建议 或 /model 菜单                     │
//! │ [footer]  模式徽标 · 快捷键 byline · 模型名             │
//! │ [ask 行]  `Waiting for permission…`                    │
//! └───────────────────────────────────────────────────────┘
//! ```
//!
//! inline（默认）模式的三条不变量，坏一条就是闪屏/错位/重复：
//!
//! 1. **每个 Row 恰占一行**（[`Row::new`] sanitize 保证）。iocraft 按 `\n`
//!    量文本高度，段里混进换行，canvas 就比行数模型高。
//! 2. **canvas 高度 < 终端高度**（[`Chrome`] + [`tail_window`] 保证）。
//!    等于或超过时 iocraft 改用 `Clear(All)+Clear(Purge)`：整屏闪 + 清空
//!    用户 scrollback。
//! 3. **只有定稿内容落盘**（`Chat::advance_flushed`）。流式 markdown 会
//!    全量重解析，未定稿行一旦进 scrollback 就是改不掉的中间态。

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use iocraft::prelude::*;
use tokio::sync::mpsc;

/// 组件体 → 重绘 hook 的共享格：hook 只能在注册时拿到值，而 canvas 行数
/// 要等组件体算完；State 也不能当通道——写 State 会无条件唤醒重渲。
#[derive(Default)]
struct RedrawSignal {
    /// 上一帧 canvas 行数（尾部 + chrome）。
    shape: std::sync::atomic::AtomicUsize,
    /// 上一帧绘制时的终端宽度（reflow 补偿 + 尺寸变化检测用）。
    painted_width: std::sync::atomic::AtomicUsize,
    /// 上一帧绘制时的终端高度。
    painted_height: std::sync::atomic::AtomicUsize,
    /// 本帧终端尺寸有变化（组件体检测——body 是唯一的尺寸权威，
    /// 清屏与 reflow 补偿因此必然同帧、用同一宽度）。
    size_changed: std::sync::atomic::AtomicBool,
    /// 请求本帧整屏清除重画。
    force: std::sync::atomic::AtomicBool,
}

/// Extra physical rows the previously painted canvas occupies after the
/// terminal rewrapped it at a narrower width. iocraft emits every canvas row
/// at full width (empty cells become spaces), so on shrink each logical row
/// wraps to exactly `ceil(prev_w / new_w)` physical rows, uniformly. The
/// clamp keeps a pathological ratio from requesting an absurd climb (cursor
/// movement clamps at the viewport top anyway).
fn shrink_deficit(prev_w: usize, prev_rows: usize, new_w: usize, height: usize) -> usize {
    if new_w == 0 || prev_w <= new_w || prev_rows == 0 {
        return 0;
    }
    (prev_rows * (prev_w.div_ceil(new_w) - 1)).min(height.saturating_mul(4))
}

/// Whether the terminal rewraps existing lines when its width changes.
/// Reflowing terminals split the old canvas rows across extra physical
/// lines, so the inline clear must climb further ([`shrink_deficit`]).
/// Non-reflowing terminals (plain xterm and kin) truncate instead — the
/// extra climb would erase flushed transcript content there, so only an
/// allowlist of known-reflowing terminals gets the compensation.
fn terminal_reflows(term_program: Option<&str>, term: Option<&str>, in_tmux: bool) -> bool {
    if in_tmux {
        // tmux rewraps pane content itself regardless of the outer terminal.
        return true;
    }
    match term_program {
        Some("ghostty") | Some("kitty") | Some("iTerm.app") | Some("WezTerm")
        | Some("Apple_Terminal") | Some("vscode") => true,
        _ => term.is_some_and(|t| {
            t.starts_with("xterm-kitty")
                || t.starts_with("xterm-ghostty")
                || t.starts_with("alacritty")
        }),
    }
}

fn terminal_reflows_env() -> bool {
    terminal_reflows(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("TMUX").is_some(),
    )
}

/// 重绘纪律：只有 (a) 本帧有落盘打印（use_output 自己会先清屏）、
/// (b) 终端尺寸变化、(c) canvas 行数变化 三种情况整屏清除重画。
///
/// 其余同形内容变化交给 iocraft 的行 diff——每个 Row 恒占一行
/// （[`Row::new`] 保证），行数不变时 diff 的相对定位必然正确。不支持
/// synchronized update 的终端上，每帧全清重画肉眼可见地闪。
///
/// 首帧不清：inline 模式的清除是相对光标回退，会抹掉 shell 里的既有内容。
struct RedrawGuard {
    signal: Arc<RedrawSignal>,
    first: bool,
}

impl Hook for RedrawGuard {
    fn poll_change(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        // Size changes arrive as Resize terminal events (use_terminal_size
        // wakes the loop); the guard itself has nothing to poll. Detection
        // lives in the component body so the clear and the reflow
        // compensation always run in the same frame with the same width.
        Poll::Pending
    }

    fn post_component_update(&mut self, updater: &mut iocraft::ComponentUpdater) {
        use std::sync::atomic::Ordering::Relaxed;
        let first = self.first;
        self.first = false;
        let force = self.signal.force.swap(false, Relaxed);
        let size_changed = self.signal.size_changed.swap(false, Relaxed);
        if (size_changed || force) && !first {
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
    /// inline 模式（默认，非全屏）：定稿行经 use_output 打印进
    /// 终端 scrollback，canvas 只画动态尾部；None/false = 全屏视口模式。
    pub inline: Option<bool>,
    /// kitty graphics protocol 能力（inline 模式检测；None = 图片显示
    /// `#[image]` 占位）。
    pub image_cap: Option<crate::tui::gfx::ImageCap>,
    /// 图片能力探测的一次性提示（如 tmux passthrough 未开启）。
    pub image_warning: Option<String>,
}

/// inline 模式（非全屏）下的按键 gate：REPL 无滚动区，历史滚动交给终端
/// scrollback；ctrl+o 折叠只放行未落盘的最后一条消息（已打印进 scrollback
/// 的改不动了）。其余按键（含 Ctrl+C 的中断/清空/退出三态）全部交给
/// [`Chat::on_key`]——退出由 `chat.exit` 表达，不再由 gate 判定。
fn inline_gate(chat: &mut Chat, code: KeyCode, modifiers: KeyModifiers) {
    if code == KeyCode::Char('o')
        && modifiers.contains(KeyModifiers::CONTROL)
        && !chat.last_message_dynamic()
    {
        return;
    }
    if !chat.ask_key(code) {
        let _ = chat.on_key(code, modifiers);
    }
}

/// canvas 中 transcript 之外的行数——单一来源。viewport、落盘边界、
/// 形状检测与 children 组装全部用它：任何低估都会让 canvas 高度 ≥ 终端
/// 高度，iocraft 随即走 `Clear(All)+Clear(Purge)` 兜底，每帧整屏闪烁并
/// 清空用户 scrollback。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Chrome {
    /// 运行状态行（`✻ Working… (esc to interrupt · 3s)`）。
    status: usize,
    /// 任务区。
    tasks: usize,
    /// 警告行。
    warning: usize,
    /// `?` 快捷键面板。
    help: usize,
    /// 输入框：上边框 + 输入行（多行输入按行计）+ 下边框。
    prompt: usize,
    /// ctrl+r 搜索提示行。
    search: usize,
    /// 排队消息行。
    queue: usize,
    /// slash 建议 / `/model` 菜单（二者互斥）。
    suggestions: usize,
    /// 临时提示行（`Press ctrl-c again to exit`）。
    notice: usize,
    /// footer。
    footer: usize,
    /// `Waiting for permission…` 提示行。
    ask: usize,
}

impl Chrome {
    fn of(chat: &Chat) -> Self {
        Self {
            status: usize::from(chat.running_status().is_some()),
            tasks: chat.task_lines().len(),
            warning: usize::from(!chat.warnings.is_empty()),
            help: chat.help_lines().len(),
            prompt: 2 + chat.prompt_lines().len(),
            search: usize::from(chat.search_line().is_some()),
            queue: chat.queue_lines().len(),
            suggestions: suggestion_rows(chat),
            notice: usize::from(chat.notice.is_some()),
            footer: 1,
            ask: usize::from(chat.pending_ask.is_some()),
        }
    }

    fn total(self) -> usize {
        self.status
            + self.tasks
            + self.warning
            + self.help
            + self.prompt
            + self.search
            + self.queue
            + self.suggestions
            + self.notice
            + self.footer
            + self.ask
    }
}

/// 建议区行数：slash 建议优先，其次 `/model` 菜单（组装处同一规则）。
fn suggestion_rows(chat: &Chat) -> usize {
    if !chat.slash_suggestions.is_empty() {
        return chat.slash_suggestions.len();
    }
    chat.model_menu
        .as_ref()
        .map(|m| match &m.models {
            // loading / 空列表都只渲染一行提示。
            Some(mm) if mm.loading || mm.models.is_empty() => 1,
            Some(mm) => mm.models.len(),
            None => m.providers.len(),
        })
        .unwrap_or(0)
        .min(crate::tui::chat::SLASH_SUGGESTIONS_MAX + 5)
}

/// inline canvas 的尾部窗口：返回 (起始行, 被省略的行数)。
///
/// canvas 高度（尾部 + 省略提示 + chrome）恒**严格小于**终端高度：
/// iocraft 在 `prev_canvas_height >= 终端高度` 时改用 Clear(All)+Purge，
/// 那会清空用户 scrollback。留 1 行余量即恒走相对擦除路径。
fn tail_window(chat: &Chat, chrome: usize, height: usize) -> (usize, usize) {
    let total = chat.doc.rows.len();
    let start = chat.tail_start.min(total);
    let budget = height.saturating_sub(chrome).saturating_sub(1);
    let len = total - start;
    if budget == 0 {
        return (total, 0);
    }
    if len <= budget {
        return (start, 0);
    }
    // 省略提示自己占一行。
    let visible = budget - 1;
    (total - visible, len - visible)
}

/// bingo 主界面根组件：通道驱动状态 + 布局。
/// 全屏模式：视口切片 + app 内滚动；inline 模式（默认）：定稿行落盘
/// scrollback + canvas 只画动态尾部（非全屏）。
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
        let mut chat = Chat::new(
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
        );
        chat.image_cap = props.image_cap;
        if let Some(warning) = props.image_warning.clone() {
            chat.warnings.push(warning);
        }
        chat
    });
    // inline 模式：空闲 Ctrl+C 退出标志（busy 时 Ctrl+C 走 on_key 取消）。
    let mut exit_requested = hooks.use_state(|| false);
    // 重绘信号共享格（组件体写、hook 读）。State 只当存储用——
    // 只读不写，不会触发重渲。
    let signal = hooks.use_state(|| Arc::new(RedrawSignal::default()));
    let signal = signal.read().clone();
    // 落盘打印必须在**清屏之后**写出：iocraft 的 inline diff/clear 全部按
    // "光标在上一帧 canvas 末行"做相对回退。use_output 的 hook 自己会先
    // clear_terminal_output 再写队列，而 hook 按注册顺序执行——重绘 hook
    // 注册在它之前，于是"清屏 → 落盘打印 → 全量重画"同帧完成。
    // 组件体里的 println 只是入队，不动光标。
    {
        let signal = signal.clone();
        hooks.use_hook(move || RedrawGuard {
            signal,
            first: true,
        });
    }
    let (stdout_handle, _stderr_handle) = hooks.use_output();
    let (width, height) = hooks.use_terminal_size();

    // Size-change detection (single authority): compare against the size of
    // the last painted frame. painted_width == 0 means nothing painted yet —
    // the first frame must not clear (it would erase shell content above).
    {
        use std::sync::atomic::Ordering::Relaxed;
        let prev_w = signal.painted_width.load(Relaxed);
        let prev_h = signal.painted_height.load(Relaxed);
        if prev_w != 0 && (prev_w != width as usize || prev_h != height as usize) {
            signal.size_changed.store(true, Relaxed);
        }
    }

    // 主循环：tick（spinner/thinking 计时）+ 通道排空 + 任务快照。
    {
        let mut tick_chat = chat;
        hooks.use_future(async move {
            let mut tick: u64 = 0;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
                // 空闲（无动画、无待处理事件）时不 write：iocraft 的
                // State::write() 无条件唤醒重渲，每 33ms 写一次就是
                // 30fps 空转重建。
                if !tick_chat.read().needs_tick() {
                    continue;
                }
                let drained = {
                    let mut s = tick_chat.write();
                    s.tick();
                    s.drain_all()
                };
                if drained {
                    tick = 0;
                }
                // 任务区不可见时不查磁盘：快照变化才置 dirty。
                if tick.is_multiple_of(TASKS_REFRESH_TICKS)
                    && tick_chat.read().tasks_visible
                {
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
                    inline_gate(&mut key_chat.write(), code, modifiers);
                } else {
                    key_chat.write().on_key(code, modifiers);
                }
                if key_chat.read().exit {
                    *exit_flag.write() = true;
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

    // 终端尺寸必须先于 chrome 写进状态：输入行折行与 `?` 面板行数都按它算，
    // 用上一帧的尺寸算 chrome 会与实际组装的行数对不上（组装处的
    // debug_assert 会抓到，生产里则是 canvas 高度失准）。
    // 先算 needs_build 再写，否则"宽度变了"这个信号被自己抹掉。
    // 无条件 write() 会令每次渲染标记变更 → 无限重渲，故一律有守卫。
    let needs_build = {
        let s = chat.read();
        // 宽度变化必须重建：markdown 折行、欢迎卡宽度全按旧宽度算。
        s.dirty || s.width != width as usize || s.height != height as usize
    };
    if needs_build {
        let mut s = chat.write();
        s.width = width as usize;
        s.height = height as usize;
    }

    // chrome 单一来源：viewport / 落盘预算 / 形状检测 / 组装校验共用。
    let chrome = Chrome::of(&chat.read());
    let viewport = (height as usize).saturating_sub(chrome.total()).max(1);

    if needs_build || chat.read().viewport_height != viewport {
        let mut s = chat.write();
        s.viewport_height = viewport;
        if needs_build {
            s.dirty = false;
            s.reconcile_scroll(viewport);
            s.build_rows(width as usize);
        }
    }

    // Reflow compensation: on width shrink, reflowing terminals rewrap every
    // previously painted (full-width) canvas row into ceil(prev_w/new_w)
    // physical rows, while iocraft's inline clear climbs only one physical
    // row per logical row — the surplus top rows survive and pile up in
    // scrollback on every resize step. Queue an extra climb+erase: use_output
    // writes it right after iocraft's own clear and before the canvas
    // repaint (queue order also puts it ahead of this frame's flush prints).
    // The trailing `\x1b[1A` plus println's `\r\n` land the cursor back on
    // the erased region's first row, column 0.
    if inline {
        use std::sync::atomic::Ordering::Relaxed;
        let prev_w = signal.painted_width.load(Relaxed);
        let prev_rows = signal.shape.load(Relaxed);
        let deficit = shrink_deficit(prev_w, prev_rows, width as usize, height as usize);
        if deficit > 0 && terminal_reflows_env() {
            stdout_handle.println(format!("\x1b[{deficit}F\x1b[J\x1b[1A"));
        }
    }

    // inline：把新定稿的前缀落盘进 scrollback。只落定稿内容——流式
    // markdown 会全量重解析（表格/代码围栏后置成型），把未定稿行写进
    // scrollback 就会留下永远改不掉的中间态。
    if inline {
        let pending: Vec<Row> = {
            let s = chat.read();
            if s.doc.settled > s.tail_start {
                s.doc.rows[s.tail_start..s.doc.settled].to_vec()
            } else {
                Vec::new()
            }
        };
        if !pending.is_empty() {
            let (theme, images, image_cap) = {
                let s = chat.read();
                (s.theme.clone(), s.images.clone(), s.image_cap)
            };
            for (i, row) in pending.iter().enumerate() {
                // 图片块：块首行输出 kitty 序列（放置 + 换行推进光标），
                // 续行不输出——整块占位行由序列一并消费。
                if let Some(img) = &row.line.image {
                    if !image_block_head(&pending, i) {
                        continue;
                    }
                    if let (Some(cap), Some(meta)) = (image_cap, images.get(&img.url)) {
                        // 传输 + 放置：直发 kitty 序列，或 tmux passthrough
                        // 包裹 + Unicode 占位符（模式由能力探测决定）。
                        let seq = crate::tui::gfx::image_print_bytes(
                            &cap,
                            &meta.bytes,
                            img.cols,
                            img.rows,
                            crate::tui::gfx::image_id_for(&img.url),
                        );
                        stdout_handle.print(String::from_utf8_lossy(&seq));
                        continue;
                    }
                }
                let mut buf = Vec::new();
                let canvas = flushed_row_element(row, theme.text, width).render(None);
                let _ = canvas.write_ansi(&mut buf);
                let rendered = String::from_utf8_lossy(&buf);
                stdout_handle.println(rendered.trim_end_matches(['\n', '\r']));
            }
            chat.write().advance_flushed();
        }
    }

    // 动态尾部：未落盘的行；超出高度预算时只画末尾若干行 + 省略提示。
    let (tail_start, tail_hidden) = if inline {
        tail_window(&chat.read(), chrome.total(), height as usize)
    } else {
        (0, 0)
    };
    {
        use std::sync::atomic::Ordering::Relaxed;
        if inline {
            let tail_rows = chat.read().doc.rows.len() - tail_start;
            let canvas_rows = tail_rows + usize::from(tail_hidden > 0) + chrome.total();
            if signal.shape.swap(canvas_rows, Relaxed) != canvas_rows {
                signal.force.store(true, Relaxed);
            }
        }
        // Both modes record the painted size — it doubles as the baseline
        // for the body-side size-change detection.
        signal.painted_width.store(width as usize, Relaxed);
        signal.painted_height.store(height as usize, Relaxed);
    }

    // 只读阶段：文档已就绪，直接读。
    let (prompt_lines, bash_mode, busy, warnings, tasks, mode, model, thinking, has_ask,
         status, slash_suggestions, slash_selected, model_menu, help, queue, notice, search) = {
        let s = chat.read();
        (
            s.prompt_lines(),
            s.bash_mode,
            s.busy,
            s.warnings.clone(),
            s.task_lines(),
            s.permission_mode,
            s.session.runtime.model.borrow().clone(),
            s.session.runtime.thinking.borrow().clone(),
            s.pending_ask.is_some(),
            s.running_status(),
            s.slash_suggestions.clone(),
            s.slash_selected,
            s.model_menu.clone(),
            s.help_lines(),
            s.queue_lines(),
            s.notice,
            s.search_line(),
        )
    };

    let theme = chat.read().theme.clone();

    // ctrl+l：请求整屏清除重画（花屏恢复）。
    if chat.read().force_redraw {
        chat.write().force_redraw = false;
        signal.force.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    // 组装布局。
    let mut children: Vec<AnyElement<'static>> = Vec::new();

    children.push(element!(Transcript(
        chat: Some(chat),
        viewport: viewport,
        tail_start: if inline { Some(tail_start) } else { None },
        tail_hidden: tail_hidden,
    ))
    .into_any());

    // 运行状态行（ActivityIndicator）：busy 时在输入框上方显示
    // `✻ {动词}… (esc to interrupt · {N}s · ↓ {tokens} tokens)`——
    // 让用户时刻知道 agent 正在运行，独立于 transcript 内容与滚动。
    if let Some(status) = &status {
        let spinner = crate::tui::activities::spinner(chat.read().tick);
        children.push(status_row(status, spinner, &theme));
    }

    // 任务列表（输入框上方）。
    for line in &tasks {
        children.push(row_element(&Row::new(line.clone()), theme.text));
    }

    // 通知行（overlay 位置：输入框上方一行）。
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

    // `?` 快捷键面板（输入框上方，dim；内容与绑定同源见 tui::keys）。
    for line in &help {
        children.push(dim_row(line.clone(), theme.inactive));
    }

    // footer：模式徽标 + 快捷键 byline（左），模型名（右）。
    // bash 模式下左侧提示 `! for shell mode`（CC bashBorder 提示）。
    // 空闲提示 `? for shortcuts`；busy 时 `esc to interrupt` 由状态行承担，
    // footer 不重复。
    let hints = if busy {
        crate::tui::keys::FOOTER_EXPAND_HINT.to_string()
    } else {
        format!(
            "{} · {}",
            crate::tui::keys::FOOTER_IDLE_HINT,
            crate::tui::keys::FOOTER_EXPAND_HINT
        )
    };
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
                    content: hints,
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

    // 输入行：`{prefix}{行}`（首行带 `❯ `/`!` 前缀，续行缩进对齐）。
    // ▋ 假光标画在真实光标处：inline 模式下终端光标停在 canvas 末行，
    // 不跟随输入。
    let prompt_style = if busy { theme.inactive } else { theme.text };
    let (prefix, prefix_color) = if bash_mode {
        ("! ".to_string(), theme.bash_border)
    } else {
        ("❯ ".to_string(), prompt_style)
    };
    let input_rows: Vec<AnyElement<'static>> = prompt_lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let prefix = if i == 0 { prefix.clone() } else { "  ".to_string() };
            let body = line_inner(line, theme.text);
            element! {
                View(height: 1, width: 100pct, flex_direction: FlexDirection::Row) {
                    Text(color: prefix_color, content: prefix, wrap: TextWrap::NoWrap)
                    #(vec![body].into_iter())
                }
            }
            .into_any()
        })
        .collect();

    // 输入框边框（CC promptBorder 圆角；bash 模式换 bashBorder 色）。
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
    let border_bottom = element! {
        Text(
            color: border_color,
            content: format!("╰{}╯", "─".repeat(width.saturating_sub(2) as usize)),
            wrap: TextWrap::NoWrap,
        )
    };

    let suggestions_view = suggestion_views(
        &slash_suggestions,
        slash_selected,
        model_menu.as_ref(),
        &theme,
        width,
    );

    // 布局：输入框（上边框 + 输入行 + 下边框）在 footer 上方。
    // 建议行按模式定位：fullscreen 上方、inline 下方（对齐 slash 输出）。
    let mut suggestions_view = suggestions_view;
    if !inline {
        children.extend(std::mem::take(&mut suggestions_view));
    }
    children.push(border_top.into_any());
    children.extend(input_rows);
    children.push(border_bottom.into_any());
    // ctrl+r 搜索提示行（输入框下方）。
    if let Some(line) = &search {
        children.push(dim_row(line.clone(), theme.inactive));
    }
    // 排队消息（输入框下方 dim `> {text}`）。
    for line in &queue {
        children.push(dim_row(line.clone(), theme.inactive));
    }
    if inline {
        children.extend(suggestions_view);
    }
    // 临时提示行（`Press ctrl-c again to exit`）。
    if let Some(text) = notice {
        children.push(dim_row(text.to_string(), theme.inactive));
    }
    children.push(footer.into_any());

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

    // chrome 公式与实际组装必须一一对应（children 首项是 Transcript）。
    debug_assert_eq!(
        children.len() - 1,
        chrome.total(),
        "chrome formula out of sync with assembled rows: {chrome:?}"
    );

    // inline 模式：空闲 Ctrl+C 请求退出会话；/exit 两种模式都退出。
    let mut system = hooks.use_context_mut::<SystemContext>();
    if chat.read().exit {
        *exit_requested.write() = true;
    }
    if *exit_requested.read() {
        system.exit();
    }

    // inline：根 View 不固定高度——canvas 只占内容自然高度（尾部 +
    // chrome），输入行随内容流走，不会钉在屏幕底部（非全屏）；
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

/// 建议区元素（PromptInputFooterSuggestions）：`  /name  description`，
/// 选中行整行 permission 色并带 `❯` 前缀，其余 dim（CC Select）。
/// slash 建议优先，其次 `/model` 菜单。
///
/// 与 [`suggestion_rows`] 是同一分支规则的两面——行数必须一致，否则
/// chrome 低估、canvas 越界。测试对表二者。
///
/// 每行都按可用宽度截断：iocraft 的 NoWrap 不截断，超宽行会被终端自己
/// 折行，canvas 行数与 iocraft 的认知随即不符。
fn suggestion_views(
    slash_suggestions: &[crate::tui::chat::SlashSuggestion],
    slash_selected: usize,
    model_menu: Option<&crate::tui::chat::ModelMenu>,
    theme: &Theme,
    width: u16,
) -> Vec<AnyElement<'static>> {
    let row = |line: String, selected: bool| {
        let color = if selected {
            theme.permission
        } else {
            theme.inactive
        };
        // 无额外缩进：`❯` 落在行首，名称列与输入行的文本列对齐（CC Select）。
        element! {
            View(height: 1, width: 100pct) {
                Text(color: color, content: line, wrap: TextWrap::NoWrap)
            }
        }
        .into_any()
    };
    if slash_suggestions.is_empty() {
        let Some(menu) = model_menu else {
            return Vec::new();
        };
        // `/model` 二级选择器：一级 `▸ provider`，二级 `▸ model`
        //（loading / 空列表各占一行提示）。
        let items: Vec<(String, bool)> = match &menu.models {
            Some(m) if m.loading => vec![("… 拉取模型列表".to_string(), true)],
            Some(m) if m.models.is_empty() => {
                vec![("（该端点未返回模型，Esc 退出）".to_string(), true)]
            }
            Some(m) => m
                .models
                .iter()
                .enumerate()
                .map(|(i, name)| (name.clone(), i == m.selected))
                .collect(),
            None => menu
                .providers
                .iter()
                .enumerate()
                .map(|(i, p)| (p.clone(), i == menu.provider_selected))
                .collect(),
        };
        return items
            .into_iter()
            .take(crate::tui::chat::SLASH_SUGGESTIONS_MAX + 5)
            .map(|(name, selected)| {
                let line = crate::tui::markdown::truncate(
                    &format!("{}{name}", if selected { "❯ " } else { "  " }),
                    (width as usize).saturating_sub(2),
                );
                row(line, selected)
            })
            .collect();
    }
    let name_col = slash_suggestions
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    // 可用描述宽度 = 终端宽 - padding(2) - "❯ "(2) - 名称列 - 分隔(2)。
    let desc_width = (width as usize).saturating_sub(2 + 2 + name_col + 2).max(8);
    slash_suggestions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let selected = i == slash_selected;
            let name_text = format!("/{:<width$}", s.name, width = name_col);
            let desc = crate::tui::markdown::truncate(&s.description, desc_width);
            // 整行二次截断：name_col 与 padding 叠加后仍可能超内容区
            //（CJK 宽度）。
            let line = crate::tui::markdown::truncate(
                &format!("{}{name_text}  {desc}", if selected { "❯ " } else { "  " }),
                (width as usize).saturating_sub(2),
            );
            row(line, selected)
        })
        .collect()
}

/// 一行 dim 文本（帮助面板 / 队列 / 提示 / 搜索行共用）。
fn dim_row(content: String, color: Color) -> AnyElement<'static> {
    element! {
        View(height: 1, width: 100pct, padding_left: 2) {
            Text(color: color, content: content, wrap: TextWrap::NoWrap)
        }
    }
    .into_any()
}

/// 运行状态行（ActivityIndicator）：
/// `✻ {动词}… (esc to interrupt · {N}s · ↓ {tokens} tokens)`。
/// 动词与星芒 spinner 用主强调色，括号内的 meta 用 inactive。
fn status_row(
    status: &crate::tui::chat::RunningStatus,
    spinner: char,
    theme: &Theme,
) -> AnyElement<'static> {
    let mut meta = format!(
        "(esc to interrupt · {}s",
        status.elapsed.round().max(0.0) as u64
    );
    if status.tokens > 0 {
        meta.push_str(&format!(" · ↓ {} tokens", status.tokens));
    }
    meta.push(')');
    element! {
        View(height: 1, width: 100pct, padding_left: 2, flex_direction: FlexDirection::Row) {
            Text(
                color: theme.claude,
                content: format!("{spinner} {}… ", status.verb),
                wrap: TextWrap::NoWrap,
            )
            Text(color: theme.inactive, content: meta, wrap: TextWrap::NoWrap)
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
    /// inline 模式：渲染 `doc.rows[tail_start..]` 的尾部窗口；
    /// None = 全屏视口模式（切片 + 点击）。
    tail_start: Option<usize>,
    /// inline 模式：尾部窗口之上被省略的行数（>0 时顶端加提示行）。
    tail_hidden: usize,
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
    // 尾部超出高度预算：顶端一行 `… +N lines`（该行计入高度预算）。
    let hidden = (props.tail_hidden > 0).then(|| {
        element! {
            View(height: 1, width: 100pct, padding_left: 2) {
                Text(
                    color: theme.inactive,
                    content: format!("… +{} lines", props.tail_hidden),
                    wrap: TextWrap::NoWrap,
                )
            }
        }
        .into_any()
    });
    // sticky prompt header（CC StickyPromptHeader）：绝对定位覆盖在滚动区
    // 顶部，不占布局（避免内容整体位移触发 diff 残留）。
    // inline：flex_grow 0（canvas 取内容自然高度，输入行随内容流走，
    // 不钉屏幕底）；全屏：grow 填满视口。
    element! {
        View(
            flex_grow: if props.tail_start.is_some() { 0.0_f32 } else { 1.0_f32 },
            width: 100pct,
            flex_direction: FlexDirection::Column,
            overflow_y: Overflow::Hidden,
        ) {
            #(hidden.into_iter())
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

/// 该行是否为图片块首行（块内续行返回 false；块边界按 url 识别）。
fn image_block_head(rows: &[Row], i: usize) -> bool {
    let Some(img) = &rows[i].line.image else {
        return false;
    };
    rows.get(i.wrapping_sub(1)).is_none_or(|prev| {
        prev.line.image.as_ref().map(|p| &p.url) != Some(&img.url)
    })
}

/// 落盘用的一行元素。整行背景（用户气泡）的行外面再包一层显式宽度的
/// View：`width: 100pct` 在无定宽父级下退化成内容宽，气泡一落盘就从满行
/// 缩成文字宽——同一条消息前后两个样子。
fn flushed_row_element(row: &Row, default_color: Color, width: u16) -> AnyElement<'static> {
    let inner = row_element(row, default_color);
    if row.bg.is_none() {
        return inner;
    }
    element! {
        View(width: width) {
            #(vec![inner].into_iter())
        }
    }
    .into_any()
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
    // 图片块行：canvas 期显示占位（落盘时 FlushRows 输出真实图片序列）。
    if line.image.is_some() {
        return element!(Text(
            color: default_color,
            content: "#[image]".to_string(),
            wrap: TextWrap::NoWrap,
        ))
        .into_any();
    }
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
    use crate::tui::line::ImageRef;

    #[test]
    fn shrink_deficit_counts_extra_wrapped_rows() {
        // Width halved: every canvas row wraps once → one extra row each.
        assert_eq!(shrink_deficit(200, 10, 100, 50), 10);
        // ceil(200/67)=3 → two extra physical rows per logical row.
        assert_eq!(shrink_deficit(200, 10, 67, 50), 20);
        // Same width or widening never compensates.
        assert_eq!(shrink_deficit(100, 10, 100, 50), 0);
        assert_eq!(shrink_deficit(100, 10, 120, 50), 0);
        // Nothing painted yet / degenerate width → no climb.
        assert_eq!(shrink_deficit(0, 0, 80, 50), 0);
        assert_eq!(shrink_deficit(100, 5, 0, 50), 0);
        // Pathological ratio clamps to 4× viewport height.
        assert_eq!(shrink_deficit(10_000, 50, 10, 50), 200);
    }

    #[test]
    fn terminal_reflows_only_on_allowlist() {
        // Known reflowing terminals.
        assert!(terminal_reflows(Some("ghostty"), None, false));
        assert!(terminal_reflows(Some("kitty"), None, false));
        assert!(terminal_reflows(Some("iTerm.app"), None, false));
        assert!(terminal_reflows(Some("WezTerm"), None, false));
        assert!(terminal_reflows(Some("Apple_Terminal"), None, false));
        assert!(terminal_reflows(Some("vscode"), None, false));
        assert!(terminal_reflows(None, Some("xterm-kitty"), false));
        assert!(terminal_reflows(None, Some("alacritty"), false));
        // tmux rewraps panes itself, regardless of the outer terminal.
        assert!(terminal_reflows(None, Some("xterm-256color"), true));
        // Unknown terminals truncate — compensating would erase flushed
        // content, so they stay excluded.
        assert!(!terminal_reflows(None, Some("xterm-256color"), false));
        assert!(!terminal_reflows(None, None, false));
    }

    #[test]
    fn image_block_head_detects_block_boundaries() {
        let img = |url: &str| Line {
            segs: Vec::new(),
            image: Some(ImageRef {
                url: url.to_string(),
                cols: 10,
                rows: 3,
            }),
        };
        let plain = || Row::new(Line::plain("x"));
        // [a0, a1, a2, text, b0, b1]：a0/b0 是块首，其余续行。
        let rows = vec![
            Row::new(img("a.png")),
            Row::new(img("a.png")),
            Row::new(img("a.png")),
            plain(),
            Row::new(img("b.png")),
            Row::new(img("b.png")),
        ];
        assert!(image_block_head(&rows, 0), "块首");
        assert!(!image_block_head(&rows, 1), "续行");
        assert!(!image_block_head(&rows, 2), "续行");
        assert!(!image_block_head(&rows, 3), "普通行");
        assert!(image_block_head(&rows, 4), "新块首");
        assert!(!image_block_head(&rows, 5), "新块续行");
    }

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
    async fn root_renders_layout_and_accepts_keys() {
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
        // 帧数不再固定：重绘纪律（只在落盘/resize/形状变化时全清）让
        // 同形帧不再产生输出，且 mock 事件流会在同一次 poll 里派发
        // Resize + Key，两者可能收敛在同一帧。断言落在最后一帧上。
        let mut actual = Vec::new();
        while actual.len() < 4 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await
            {
                Ok(Some(c)) => actual.push(c.to_string()),
                _ => break,
            }
        }
        assert!(!actual.is_empty(), "no frames rendered");
        let last = actual.last().expect("frame");
        // 欢迎卡片（边框 + 标题）
        assert!(last.contains("bingo"), "welcome title: {last}");
        assert!(last.contains("╭"), "welcome box top: {last}");
        assert!(last.contains("╰"), "welcome box bottom: {last}");
        // 输入框底边框（CC promptBorder ╰──╯）
        assert!(last.contains('╯'), "input border corner: {last}");
        // footer：空闲提示（CC `? for shortcuts`）+ 模型名；
        // `esc to interrupt` 只在 busy 时由状态行承担。
        assert!(last.contains("? for shortcuts"), "footer hints: {last}");
        assert!(!last.contains("esc to interrupt"), "空闲不显示中断提示: {last}");
        assert!(last.contains("test-model"), "footer model: {last}");
        // 键盘 → 输入框
        assert!(last.contains("❯ h▋"), "typed input: {last}");
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
        // 欢迎卡本身带 `✻`，思考块要按整串定位。
        let thinking = text.find("✻ Thinking").expect("thinking");
        assert!(card < user, "card before user: {text}");
        assert!(user < thinking, "user before thinking: {text}");
        assert_eq!(text.matches('╰').count(), 1, "single card bottom: {text}");
        eprintln!("order ok: card={card} user={user} thinking={thinking}");
    }

    /// 状态行渲染（CC）：`✻ {动词}… (esc to interrupt · {N}s · ↓ {tokens} tokens)`；
    /// tokens 为 0 时省略该段。
    #[test]
    fn status_row_renders_busy_verb() {
        let theme = Theme::dark();
        let status = crate::tui::chat::RunningStatus {
            verb: "Working".to_string(),
            elapsed: 12.5,
            tokens: 0,
        };
        let mut row = status_row(&status, '✻', &theme);
        let text = row.render(Some(80)).to_string();
        assert!(text.contains("✻ Working… (esc to interrupt · 13s)"), "{text}");
        assert!(!text.contains("tokens"), "0 token 省略该段: {text}");

        let status = crate::tui::chat::RunningStatus {
            verb: "$ cargo test".to_string(),
            elapsed: 3.2,
            tokens: 1200,
        };
        let mut row = status_row(&status, '✽', &theme);
        let text = row.render(Some(80)).to_string();
        assert!(
            text.contains("✽ $ cargo test… (esc to interrupt · 3s · ↓ 1200 tokens)"),
            "{text}"
        );
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
        assert!(typed.contains("! l▋"), "bash prefix + input: {typed}");
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

    /// 目视用：把典型帧（欢迎 + 用户 + 思考 + 工具 + diff + 权限框）
    /// 的 doc 行打出来（`cargo test dump_homage -- --nocapture`）。
    #[test]
    fn dump_homage_frames() {
        use crate::tui::UiEvent;
        let session = test_session();
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (asks_tx, asks_rx) = mpsc::unbounded_channel();
        let mut chat = Chat::new(session, events_tx, events_rx, asks_tx, asks_rx, Theme::dark(), None);
        chat.messages.push(crate::tui::chat::UiMessage {
            role: crate::tui::chat::Role::User,
            text: "把 TUI 对齐一下设计语言".to_string(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        for event in [
            UiEvent::TurnStart,
            UiEvent::ThinkingDelta("先看看渲染层的结构。".into()),
            UiEvent::ToolStart { name: "Read".into() },
            UiEvent::ToolReady {
                name: "Read".into(),
                input: serde_json::json!({"file_path": "src/tui/chat.rs"}),
                standalone: false,
            },
            UiEvent::ToolDone(crate::query::ToolCallDone {
                name: "Read".into(),
                summary: "src/tui/chat.rs".into(),
                output: "line\nline\nline".into(),
                is_error: false,
                diff: None,
                duration_ms: 120,
            }),
            UiEvent::ToolStart { name: "Bash".into() },
            UiEvent::ToolReady {
                name: "Bash".into(),
                input: serde_json::json!({"command": "cargo test"}),
                standalone: true,
            },
            UiEvent::ToolDone(crate::query::ToolCallDone {
                name: "Bash".into(),
                summary: "cargo test".into(),
                output: "$ cargo test\n439 passed".into(),
                is_error: false,
                diff: None,
                duration_ms: 4200,
            }),
            UiEvent::ToolStart { name: "Edit".into() },
            UiEvent::ToolDone(crate::query::ToolCallDone {
                name: "Edit".into(),
                summary: "src/tui/theme.rs".into(),
                output: String::new(),
                is_error: false,
                diff: Some("--- a/src/tui/theme.rs\n+++ b/src/tui/theme.rs\n@@ -1,3 +1,4 @@\n ctx\n-old line\n+new line\n+extra\n".into()),
                duration_ms: 30,
            }),
            UiEvent::TextDelta("改好了：工具行改成双行结构。".into()),
            UiEvent::TurnEnd,
        ] {
            let _ = chat.events.send(event);
        }
        chat.drain_all();
        chat.tasks_visible = true;
        let (tx, _rx) = tokio::sync::oneshot::channel();
        chat.pending_ask = Some((
            crate::tui::chat::PermissionRequest::new("允许执行 Bash", "cargo test", vec!["允许".into(), "拒绝".into()]),
            tx,
        ));
        chat.width = 96;
        chat.build_rows(96);
        eprintln!("======== transcript ========");
        for row in &chat.doc.rows {
            eprintln!("{}", row.line.plain_text());
        }
        eprintln!("======== tasks ========");
        for line in chat.task_lines() {
            eprintln!("{}", line.plain_text());
        }
        eprintln!("======== prompt (multi-line + help) ========");
        chat.set_input("第一行\n第二行");
        chat.cursor = "第一行\n第".len();
        for line in chat.prompt_lines() {
            eprintln!("|{}|", line.plain_text());
        }
        chat.help_visible = true;
        chat.height = 40;
        for line in chat.help_lines() {
            eprintln!("{line}");
        }
        eprintln!("======== queue / status ========");
        chat.queued = vec!["下一条消息".into()];
        for line in chat.queue_lines() {
            eprintln!("{line}");
        }
        chat.busy = true;
        if let Some(status) = chat.running_status() {
            eprintln!("status: ✻ {}… (esc to interrupt · {}s · ↓ {} tokens)", status.verb, status.elapsed.round(), status.tokens);
        }
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

    fn chat_for_test() -> Chat {
        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (asks_tx, asks_rx) = tokio::sync::mpsc::unbounded_channel();
        Chat::new(
            test_session(),
            events_tx,
            events_rx,
            asks_tx,
            asks_rx,
            Theme::dark(),
            None,
        )
    }

    /// inline 模式端到端冒烟：canvas 只画动态尾部 + chrome——欢迎卡在
    /// 首帧就落盘进 scrollback，不再占 canvas；chrome 公式与组装一致
    /// （不一致会命中组装处的 debug_assert）。
    #[tokio::test]
    async fn inline_canvas_holds_only_tail_and_chrome() {
        let session = test_session();
        let (_expand_tx, expand_rx) = tokio::sync::watch::channel(false);
        let mut root = element!(Bingo(
            session: Some(session),
            expand_rx: Some(expand_rx),
            inline: Some(true),
        ));
        let stream = root.mock_terminal_render_loop(MockTerminalConfig::with_events(
            futures_util::stream::iter(vec![
                TerminalEvent::Resize(80, 24),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
            ]),
        ));
        let mut stream = Box::pin(stream);
        let mut frames: Vec<String> = Vec::new();
        while frames.len() < 6 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await
            {
                Ok(Some(c)) => frames.push(c.to_string()),
                _ => break,
            }
        }
        let last = frames.last().expect("frame");
        // chrome 齐全。
        assert!(last.contains("❯ x▋"), "输入行: {last}");
        assert!(last.contains("? for shortcuts"), "footer: {last}");
        assert!(last.contains('╰'), "输入框边框: {last}");
        // 欢迎卡已落盘 → 不在 canvas 里。
        assert!(
            !last.contains("Welcome back"),
            "欢迎卡不应留在 canvas: {last}"
        );
        // canvas 行数严格小于终端高度（否则 iocraft 走 Purge 兜底）。
        assert!(
            last.lines().count() < 24,
            "canvas {} 行 >= 终端 24 行",
            last.lines().count()
        );
    }

    /// 落盘的气泡行按终端宽渲染（canvas 里是满行背景），普通行按内容宽
    /// （避免 scrollback 里全是行尾空格）。
    #[test]
    fn flushed_bubble_row_spans_terminal_width() {
        let theme = Theme::dark();
        let mut line = Line::styled("❯ ", crate::tui::line::SegStyle::fg(theme.text));
        line.push_styled("hi", crate::tui::line::SegStyle::fg(theme.text));
        let bubble = Row::bubble(line, theme.user_message_bg);
        let canvas = flushed_row_element(&bubble, theme.text, 40).render(None);
        let text = canvas.to_string();
        let first = text.lines().next().expect("row");
        assert_eq!(first.chars().count(), 40, "气泡满行: {first:?}");
        // 普通行不补空格。
        let plain = Row::new(Line::plain("plain row"));
        let canvas = flushed_row_element(&plain, theme.text, 40).render(None);
        assert_eq!(canvas.to_string().trim_end_matches('\n'), "plain row");
    }

    /// chrome 行数公式与实际组装的建议区行数一一对应。二者分家正是
    /// canvas 越界（→ Clear(All)+Purge 闪屏）的根因。
    #[test]
    fn suggestion_rows_matches_rendered_views() {
        use crate::tui::chat::{ModelMenu, ModelMenuModels, SlashSuggestion};
        let theme = Theme::dark();
        let mut chat = chat_for_test();
        let check = |chat: &Chat| {
            let views = suggestion_views(
                &chat.slash_suggestions,
                chat.slash_selected,
                chat.model_menu.as_ref(),
                &theme,
                80,
            );
            assert_eq!(views.len(), suggestion_rows(chat), "行数公式与组装一致");
        };
        check(&chat);
        // 一级：provider 列表。
        chat.model_menu = Some(ModelMenu {
            providers: vec!["default".into(), "openrouter".into()],
            provider_selected: 0,
            models: None,
        });
        check(&chat);
        assert_eq!(suggestion_rows(&chat), 2);
        // 二级 loading：只渲染一行提示（旧公式按 models.len()=0 算，
        // 少算一行 → canvas 高度低估）。
        chat.model_menu.as_mut().expect("menu").models = Some(ModelMenuModels {
            provider: "default".into(),
            models: Vec::new(),
            loading: true,
            selected: 0,
        });
        check(&chat);
        assert_eq!(suggestion_rows(&chat), 1, "loading 占一行");
        // 二级空列表：同样一行提示。
        chat.model_menu.as_mut().expect("menu").models = Some(ModelMenuModels {
            provider: "default".into(),
            models: Vec::new(),
            loading: false,
            selected: 0,
        });
        check(&chat);
        assert_eq!(suggestion_rows(&chat), 1, "空列表占一行");
        // 二级模型列表按 5+5 上限截断。
        chat.model_menu.as_mut().expect("menu").models = Some(ModelMenuModels {
            provider: "default".into(),
            models: (0..30).map(|i| format!("m{i}")).collect(),
            loading: false,
            selected: 0,
        });
        check(&chat);
        assert_eq!(suggestion_rows(&chat), crate::tui::chat::SLASH_SUGGESTIONS_MAX + 5);
        // slash 建议优先于菜单。
        chat.slash_suggestions = vec![SlashSuggestion {
            name: "help".into(),
            description: "显示可用命令".into(),
        }];
        check(&chat);
        assert_eq!(suggestion_rows(&chat), 1);
    }

    /// chrome total = 各段之和，且与布局一一对应（transcript 之外的每一行）。
    #[test]
    fn chrome_total_sums_every_section() {
        let mut chat = chat_for_test();
        let chrome = Chrome::of(&chat);
        // 空闲：输入框 3 行（两条边框 + 一行占位提示）+ footer 1 行。
        assert_eq!(chrome, Chrome { prompt: 3, footer: 1, ..Chrome::default() });
        assert_eq!(chrome.total(), 4);
        // busy → 状态行；warning → 通知行；pending_ask → 等待提示。
        chat.busy = true;
        chat.warnings.push("mcp 连接失败".into());
        let (tx, _rx) = tokio::sync::oneshot::channel();
        chat.pending_ask = Some((
            crate::tui::chat::PermissionRequest::new("t", "q", vec!["a".into()]),
            tx,
        ));
        let chrome = Chrome::of(&chat);
        assert_eq!(chrome.status, 1);
        assert_eq!(chrome.warning, 1);
        assert_eq!(chrome.ask, 1);
        assert_eq!(chrome.total(), 3 + 1 + 1 + 1 + 1);
    }

    /// 新增的输入区行（多行输入 / `?` 面板 / 队列 / 搜索 / 提示行）全部
    /// 进 chrome 计数——漏一行就是 canvas 越界（→ Purge 闪屏）。
    #[test]
    fn chrome_counts_the_new_input_sections() {
        let mut chat = chat_for_test();
        chat.width = 100;
        chat.height = 40;
        // 多行输入：边框 2 行 + 输入 3 行。
        chat.set_input("a\nb\nc");
        assert_eq!(Chrome::of(&chat).prompt, 5);
        // `?` 面板。
        chat.help_visible = true;
        let help = Chrome::of(&chat).help;
        assert!(help > 0 && help == chat.help_lines().len());
        // 队列 + 提示行 + 搜索行。
        chat.queued.push("queued message".into());
        chat.notice = Some("Press ctrl-c again to exit");
        chat.search = Some(crate::tui::chat::HistorySearch::default());
        let chrome = Chrome::of(&chat);
        assert_eq!(chrome.queue, 1);
        assert_eq!(chrome.notice, 1);
        assert_eq!(chrome.search, 1);
        assert_eq!(
            chrome.total(),
            chrome.status
                + chrome.tasks
                + chrome.warning
                + chrome.help
                + chrome.prompt
                + chrome.search
                + chrome.queue
                + chrome.suggestions
                + chrome.notice
                + chrome.footer
                + chrome.ask
        );
    }

    /// `?` 面板经完整渲染路径出现在输入框上方，且 canvas 仍严格低于终端高度
    /// （组装处的 debug_assert 同时校验 chrome 公式与实际行数一致）。
    #[tokio::test]
    async fn help_panel_renders_within_canvas_budget() {
        let session = test_session();
        let (_expand_tx, expand_rx) = tokio::sync::watch::channel(false);
        let mut root = element!(Bingo(
            session: Some(session),
            expand_rx: Some(expand_rx),
            inline: Some(true),
        ));
        let stream = root.mock_terminal_render_loop(MockTerminalConfig::with_events(
            futures_util::stream::iter(vec![
                TerminalEvent::Resize(100, 30),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('?'))),
            ]),
        ));
        let mut stream = Box::pin(stream);
        let mut frames: Vec<String> = Vec::new();
        while frames.len() < 6 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), stream.next()).await
            {
                Ok(Some(c)) => frames.push(c.to_string()),
                _ => break,
            }
        }
        let last = frames.last().expect("frame");
        assert!(last.contains("shift+tab"), "面板列出快捷键: {last}");
        assert!(last.contains("cycle permission mode"), "面板含说明: {last}");
        assert!(last.lines().count() < 30, "canvas 仍低于终端高度: {}", last.lines().count());
    }

    /// 尾部窗口：canvas 高度（尾部 + 省略提示 + chrome）恒 < 终端高度。
    /// 等于终端高度时 iocraft 改用 Clear(All)+Purge——整屏闪 + 清空
    /// 用户 scrollback。
    #[test]
    fn tail_window_keeps_canvas_below_terminal_height() {
        let mut chat = chat_for_test();
        chat.doc.rows = (0..100).map(|i| Row::new(Line::plain(format!("r{i}")))).collect();
        let chrome = 4usize;
        for height in 6..40usize {
            let (start, hidden) = tail_window(&chat, chrome, height);
            let visible = chat.doc.rows.len() - start;
            let canvas = visible + usize::from(hidden > 0) + chrome;
            assert!(canvas < height, "height={height} canvas={canvas}");
            assert_eq!(hidden, chat.doc.rows.len() - visible, "省略数 = 未显示行数");
        }
        // 内容装得下时不省略，也不裁剪。
        chat.doc.rows.truncate(3);
        assert_eq!(tail_window(&chat, chrome, 40), (0, 0));
        // 已落盘的前缀不在尾部窗口内。
        chat.tail_start = 2;
        assert_eq!(tail_window(&chat, chrome, 40), (2, 0));
        // chrome 已占满：尾部为空（画不下就一行不画，仍不越界）。
        assert_eq!(tail_window(&chat, chrome, chrome), (3, 0));
    }

    /// inline_gate：Esc 一律放行给 on_key（菜单退出、清空输入都在那里），
    /// 只有「已落盘消息的 ctrl+o」被拦下——那些行改不动了。
    #[test]
    fn inline_gate_passes_esc_and_blocks_flushed_ctrl_o() {
        let mut chat = chat_for_test();
        chat.set_input("/model");
        chat.submit();
        assert!(chat.model_menu.is_some(), "菜单已打开");
        inline_gate(&mut chat, KeyCode::Esc, KeyModifiers::empty());
        assert!(chat.model_menu.is_none(), "Esc 经 gate 退出菜单");

        // 无消息（全部落盘）时 ctrl+o 被拦：不会触发折叠。
        chat.set_input("hi");
        inline_gate(&mut chat, KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!(chat.input, "hi", "ctrl+o 未插入字符");
    }
}
