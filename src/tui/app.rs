//! Event loop and frame assembly.
//!
//! Inline (default) layout, top to bottom — everything below the transcript is
//! "chrome", and the chrome rows are *built*, never predicted:
//!
//! ```text
//! [transcript]  live tail only (settled rows already went to scrollback)
//! [status]      `✻ Working… (esc to interrupt · 3s)`
//! [tasks]       todo · N/M tasks
//! [warning]     `⚠ …`
//! [help]        `?` panel
//! [prompt]      ╭──╮ / `❯ {input}▋` / ╰──╯
//! [search]      `(reverse-i-search)…`
//! [queue]       `> queued message`
//! [suggestions] slash menu / `/model` picker
//! [notice]      `Press ctrl-c again to exit`
//! [footer]      mode badge · hints · model
//! [ask]         `Waiting for permission…`
//! ```
//!
//! Two invariants carry the whole design:
//!
//! 1. **Settled rows are written once.** `chat.doc.settled` marks the prefix
//!    that can no longer change; it goes out through
//!    [`crate::tui::term::InlineTerm::insert_history`] and `advance_flushed` moves the cursor
//!    past it. Nothing above the viewport is ever repainted.
//! 2. **The frame is measured, not predicted.** [`Frame::assemble`] builds the
//!    row list and takes its length as the viewport height (clamped to
//!    terminal height − 1). There is no second chrome formula to drift out of
//!    sync with what is drawn.

use std::io::Stdout;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;

use crate::permission::PermissionMode;
use crate::tui::chat::{
    Chat, ModelMenu, Row, SettledMark, SlashSuggestion, ThinkMenu, model_footer_label,
};
use crate::tui::gfx;
use crate::tui::line::{Line, SegStyle, text_width};
use crate::tui::term::{HistoryItem, StdoutTerm};
use crate::tui::theme::Theme;
use crate::tui::view;

/// 每帧 tick 间隔（spinner/thinking 计时）。
const TICK_MS: u64 = 33;
/// 任务列表磁盘快照刷新间隔（tick 数）。
const TASKS_REFRESH_TICKS: u64 = 15;
/// Rows scrolled per mouse wheel notch (fullscreen only).
const WHEEL_ROWS: usize = 3;
/// 拖拽 resize 是事件风暴：静默这么久才应用新尺寸并重画。风暴中以旧宽
/// 作画只会往屏上叠更多错宽行（终端 reflow 会把它们折成残片）。
const RESIZE_QUIET_MS: u64 = 120;

/// 全屏宿主：现成的 ratatui Terminal。
pub type FullscreenHost = Terminal<CrosstermBackend<Stdout>>;

/// The chrome block plus the offset of the first input row inside it (the
/// caret lives there).
struct Chrome {
    rows: Vec<Row>,
    prompt_row: usize,
}

/// One assembled frame: the rows to draw and where the caret sits in them.
pub struct Frame {
    pub rows: Vec<Row>,
    pub cursor: Option<(u16, u16)>,
}

/// inline 尾部窗口：返回 (起始行, 被省略的行数)。预算是终端高度减去
/// chrome 与两行余量——视口顶端之上恒有 ≥2 行屏幕，落盘用的 DECSTBM
/// 滚动区域（要求至少两行）才永远合法（与 term.rs 的视口上限同源）。
fn tail_window(total: usize, tail_start: usize, chrome: usize, height: usize) -> (usize, usize) {
    let start = tail_start.min(total);
    let budget = height.saturating_sub(chrome).saturating_sub(2);
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

/// A dim row indented two columns (help / queue / notice / search share it).
fn dim_row(text: impl Into<String>, theme: &Theme) -> Row {
    Row::new(Line::styled(
        format!("  {}", text.into()),
        SegStyle::fg(theme.inactive),
    ))
}

/// 运行状态行（ActivityIndicator）：
/// `✻ {动词}… (esc to interrupt · {N}s · ↓ {tokens} tokens)`。
fn status_row(status: &crate::tui::chat::RunningStatus, spinner: char, theme: &Theme) -> Row {
    let mut meta = format!(
        "(esc to interrupt · {}s",
        status.elapsed.round().max(0.0) as u64
    );
    if status.tokens > 0 {
        meta.push_str(&format!(" · ↓ {} tokens", status.tokens));
    }
    meta.push(')');
    let mut line = Line::styled(
        format!("  {spinner} {}… ", status.verb),
        SegStyle::fg(theme.claude),
    );
    line.push_styled(meta, SegStyle::fg(theme.inactive));
    Row::new(line)
}

/// 权限模式徽标（`⏸ plan mode on`）+ 其后的 `·` 分隔符。
fn mode_badge(mode: PermissionMode, theme: &Theme) -> Vec<(String, Color)> {
    let (symbol, label, color) = match mode {
        PermissionMode::Default => return Vec::new(),
        PermissionMode::Plan => ("⏸", "plan mode on", theme.plan_mode),
        PermissionMode::AcceptEdits => ("⏵⏵", "accept edits on", theme.accept_edits),
        PermissionMode::BypassPermissions => ("⏵⏵", "bypass permissions on", theme.error),
        PermissionMode::DontAsk => ("⏵⏵", "dont ask on", theme.error),
    };
    vec![
        (format!("{symbol} {label}"), color),
        ("·".to_string(), theme.inactive),
    ]
}

/// footer：模式徽标 + 快捷键 byline（左），模型名（右）。
fn footer_row(chat: &Chat, width: usize) -> Row {
    let theme = &chat.theme;
    let hints = if chat.busy {
        crate::tui::keys::FOOTER_EXPAND_HINT.to_string()
    } else {
        format!(
            "{} · {}",
            crate::tui::keys::FOOTER_IDLE_HINT,
            crate::tui::keys::FOOTER_EXPAND_HINT
        )
    };
    let mut left = mode_badge(chat.permission_mode, theme);
    if chat.bash_mode {
        left.push(("! for shell mode".to_string(), theme.bash_border));
    }
    left.push((hints, theme.inactive));
    let model_name = chat.session.runtime.model.borrow().clone();
    let thinking = chat.session.runtime.thinking.borrow().clone();
    let model = model_footer_label(&model_name, thinking.as_deref());

    let mut line = Line::styled("  ", SegStyle::fg(theme.text));
    let mut used = 2usize;
    for (i, (text, color)) in left.iter().enumerate() {
        if i > 0 {
            line.push_styled(" ", SegStyle::fg(theme.inactive));
            used += 1;
        }
        used += text_width(text);
        line.push_styled(text.clone(), SegStyle::fg(*color));
    }
    // 右侧模型名右对齐（右侧同样留 2 列）。
    let gap = width
        .saturating_sub(used + text_width(&model) + 2)
        .max(1);
    line.push_styled(" ".repeat(gap), SegStyle::fg(theme.inactive));
    line.push_styled(model, SegStyle::fg(theme.inactive));
    Row::new(line)
}

/// 建议区：slash 建议优先，其次 `/model` 菜单，再次 `/think` 菜单。
/// 行数与内容同源——二者曾经分家，chrome 因此低估、canvas 越界。
fn suggestion_rows(
    slash: &[SlashSuggestion],
    slash_selected: usize,
    menu: Option<&ModelMenu>,
    think: Option<&ThinkMenu>,
    theme: &Theme,
    width: usize,
) -> Vec<Row> {
    let row = |line: String, selected: bool| {
        let color = if selected {
            theme.permission
        } else {
            theme.inactive
        };
        Row::new(Line::styled(line, SegStyle::fg(color)))
    };
    if slash.is_empty() {
        let Some(menu) = menu else {
            // `/think` 等级选择器（模型菜单未激活时）。
            let Some(think) = think else {
                return Vec::new();
            };
            let name_col = crate::tui::chat::THINK_LEVELS
                .iter()
                .map(|(name, _)| name.chars().count())
                .max()
                .unwrap_or(0);
            return crate::tui::chat::THINK_LEVELS
                .iter()
                .enumerate()
                .map(|(i, (name, desc))| {
                    let selected = i == think.selected;
                    let line = crate::tui::markdown::truncate(
                        &format!(
                            "{}{name:<name_col$}  {desc}",
                            if selected { "❯ " } else { "  " }
                        ),
                        width.saturating_sub(2),
                    );
                    row(line, selected)
                })
                .collect();
        };
        // `/model` 二级选择器：一级 `provider`，二级 `model`
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
                    width.saturating_sub(2),
                );
                row(line, selected)
            })
            .collect();
    }
    let name_col = slash
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    // 可用描述宽度 = 终端宽 - padding(2) - "❯ "(2) - 名称列 - 分隔(2)。
    let desc_width = width.saturating_sub(2 + 2 + name_col + 2).max(8);
    slash
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let selected = i == slash_selected;
            let name_text = format!("/{:<width$}", s.name, width = name_col);
            let desc = crate::tui::markdown::truncate(&s.description, desc_width);
            let line = crate::tui::markdown::truncate(
                &format!("{}{name_text}  {desc}", if selected { "❯ " } else { "  " }),
                width.saturating_sub(2),
            );
            row(line, selected)
        })
        .collect()
}

/// 输入框（上边框 + 输入行 + 下边框）。
fn prompt_rows(chat: &Chat, width: usize) -> Vec<Row> {
    let theme = &chat.theme;
    let border_color = if chat.bash_mode {
        theme.bash_border
    } else {
        theme.prompt_border
    };
    let prompt_style = if chat.busy { theme.inactive } else { theme.text };
    let (prefix, prefix_color) = if chat.bash_mode {
        ("! ".to_string(), theme.bash_border)
    } else {
        ("❯ ".to_string(), prompt_style)
    };
    let bar = "─".repeat(width.saturating_sub(2));
    let mut rows = vec![Row::new(Line::styled(
        format!("╭{bar}╮"),
        SegStyle::fg(border_color),
    ))];
    for (i, line) in chat.prompt_lines().into_iter().enumerate() {
        let mut row = Line::styled(
            if i == 0 { prefix.clone() } else { "  ".to_string() },
            SegStyle::fg(prefix_color),
        );
        for seg in line.segs {
            row.push_styled(seg.text, seg.style);
        }
        rows.push(Row::new(row));
    }
    rows.push(Row::new(Line::styled(
        format!("╰{bar}╯"),
        SegStyle::fg(border_color),
    )));
    rows
}

/// 输入框内的光标位置（行内偏移, 列）——与 [`Chat::prompt_lines`] 画 `▋`
/// 的位置同源。
fn caret_cell(chat: &Chat) -> (usize, usize) {
    if let Some(search) = &chat.search {
        let hit = search.hit.clone().unwrap_or_default();
        return (0, text_width(&hit).min(chat.input_width()));
    }
    if chat.input.is_empty() {
        return (0, 0);
    }
    let lines = crate::tui::input::visual_lines(&chat.input, chat.input_width());
    let (row, col) = crate::tui::input::cursor_cell(&chat.input, &lines, chat.cursor);
    let start = row.saturating_sub(crate::tui::chat::INPUT_ROWS_MAX - 1);
    (row - start, col)
}

/// transcript 之外的每一行，自上而下。`fullscreen` 只改建议区的位置
/// （全屏在输入框上方，inline 在下方，对齐 slash 输出）。
fn chrome_rows(chat: &Chat, width: usize, fullscreen: bool) -> Chrome {
    let theme = chat.theme.clone();
    let mut rows: Vec<Row> = Vec::new();

    if let Some(status) = chat.running_status() {
        rows.push(status_row(
            &status,
            crate::tui::activities::spinner(chat.tick),
            &theme,
        ));
    }
    for line in chat.task_lines() {
        rows.push(Row::new(line));
    }
    if let Some(warning) = chat.visible_warning() {
        rows.push(Row::new(Line::styled(
            format!("  ⚠ {warning}"),
            SegStyle::fg(theme.warning),
        )));
    }
    for line in chat.help_lines() {
        rows.push(dim_row(line, &theme));
    }
    // 底部实体区（agent 实例 + 频道；ctrl+g 聚焦选择）。
    for line in chat.entity_rows(width) {
        rows.push(Row::new(line));
    }

    let suggestions = suggestion_rows(
        &chat.slash_suggestions,
        chat.slash_selected,
        chat.model_menu.as_ref(),
        chat.think_menu.as_ref(),
        &theme,
        width,
    );
    if fullscreen {
        rows.extend(suggestions.iter().cloned());
    }

    let prompt = prompt_rows(chat, width);
    let prompt_row = rows.len() + 1;
    rows.extend(prompt);

    if let Some(line) = chat.search_line() {
        rows.push(dim_row(line, &theme));
    }
    for line in chat.queue_lines() {
        rows.push(dim_row(line, &theme));
    }
    if !fullscreen {
        rows.extend(suggestions);
    }
    if let Some(text) = chat.notice {
        rows.push(dim_row(text, &theme));
    }
    rows.push(footer_row(chat, width));
    if chat.pending_ask.is_some() {
        rows.push(dim_row("Waiting for permission…", &theme));
    }
    Chrome { rows, prompt_row }
}

/// #18 全流程级整屏错误态骨架（AC-26/53，ui/ux #68 规格）：错误标题 +
/// 稳定码 + 说明（发生了什么+能做什么）+ 首要动作（重试/返回）+ 退出提示。
/// 动作绑定在按键层（chat.rs：整屏态 Enter=重试、Esc=返回），本函数只画。
fn error_screen_rows(err: &crate::tui::chat::ErrorState, theme: &Theme) -> Vec<Row> {
    let mut rows = Vec::new();
    rows.push(Row::new(Line::styled(
        "⚠ 出错了",
        SegStyle::fg(theme.error).bold(),
    )));
    rows.push(Row::new(Line::styled(
        format!("[error] code={}", err.code),
        SegStyle::fg(theme.error),
    )));
    rows.push(Row::new(Line::plain(err.msg.clone())));
    rows.push(Row::new(Line::plain("")));
    rows.push(Row::new(Line::styled(
        "Enter 重试 · Esc 返回",
        SegStyle::fg(theme.inactive),
    )));
    rows
}

impl Frame {
    /// inline 帧：动态尾部（超预算时只留末尾若干行 + 省略提示）+ chrome。
    /// 行数即视口高度，故恒 ≤ 终端高度 - 2（DECSTBM 区域恒合法）。
    /// #18：全流程级错误态（`last_error.level == Full`）覆盖内容区为整屏错误，
    /// 输入光标隐藏（用户处于错误屏，首要动作由按键层处理）。
    pub fn assemble(chat: &Chat, size: Size) -> Self {
        if let Some(err) = &chat.last_error
            && err.level == crate::error::ErrorLevel::Full
        {
            return Self {
                rows: error_screen_rows(err, &chat.theme),
                cursor: None,
            };
        }
        let width = size.width as usize;
        let height = size.height as usize;
        let chrome = chrome_rows(chat, width, false);
        let (tail_start, hidden) = tail_window(
            chat.doc.rows.len(),
            chat.tail_start,
            chrome.rows.len(),
            height,
        );
        let mut rows: Vec<Row> = Vec::new();
        if hidden > 0 {
            rows.push(dim_row(format!("… +{hidden} lines"), &chat.theme));
        }
        rows.extend(chat.doc.rows[tail_start..].iter().cloned());
        let tail_len = rows.len();
        // #18 错误行（Page/Field 级别）：从结构化 `last_error` 生成，error 色
        // 高亮（A 区），追加在内容区末尾——不依赖 doc 重建、无双显。
        if let Some(err) = &chat.last_error
            && err.level != crate::error::ErrorLevel::Full
        {
            rows.push(Row::new(Line::styled(
                format!("[error] code={} msg={}", err.code, err.msg),
                SegStyle::fg(chat.theme.error),
            )));
        }
        rows.extend(chrome.rows);

        // 最后一道保险：chrome 本身也可能超过预算（很矮的终端），
        // 此时丢最上面的行——输入框与 footer 是必须留住的那部分。
        // 预算 = 高度 − 2：与 term.rs 的视口上限一致（顶上留两行，
        // DECSTBM 滚动区域恒合法）。
        let budget = height.saturating_sub(2).max(1);
        let dropped = rows.len().saturating_sub(budget);
        if dropped > 0 {
            rows.drain(..dropped);
        }
        let (caret_row, caret_col) = caret_cell(chat);
        let cursor = caret_position(
            tail_len + chrome.prompt_row + caret_row,
            caret_col + 2,
            dropped,
            rows.len(),
            width,
        );
        Self { rows, cursor }
    }
}

/// 光标格：帧顶部被裁掉 `dropped` 行之后仍落在画面里才显示。
fn caret_position(
    row: usize,
    col: usize,
    dropped: usize,
    rows: usize,
    width: usize,
) -> Option<(u16, u16)> {
    let y = row.checked_sub(dropped)?;
    if y >= rows || col >= width {
        return None;
    }
    Some((u16::try_from(col).ok()?, u16::try_from(y).ok()?))
}

/// 新定稿的行 → scrollback 条目。图片块首行发真实 kitty 字节（传输 +
/// 放置 + 光标推进），块内续行由该序列一并消费，故跳过。
fn flush_items(chat: &Chat, width: usize, end: usize) -> Vec<HistoryItem> {
    let end = end.min(chat.doc.rows.len());
    if end <= chat.tail_start {
        return Vec::new();
    }
    let pending = &chat.doc.rows[chat.tail_start..end];
    let mut items = Vec::with_capacity(pending.len());
    for (i, row) in pending.iter().enumerate() {
        if let Some(img) = &row.line.image {
            if !image_block_head(pending, i) {
                continue;
            }
            if let (Some(cap), Some(meta)) = (chat.image_cap, chat.images.get(&img.url)) {
                let bytes = gfx::image_print_bytes(
                    &cap,
                    &meta.bytes,
                    img.cols,
                    img.rows,
                    gfx::image_id_for(&img.url),
                );
                items.push(HistoryItem::Raw {
                    bytes,
                    rows: u16::try_from(img.rows).unwrap_or(u16::MAX),
                });
                continue;
            }
        }
        items.push(HistoryItem::Line(view::history_line(
            row,
            chat.theme.text,
            width,
        )));
    }
    items
}

/// 懒落盘选择：最远的一个「所属段起始行已越过窗口顶端」的定稿检查点。
/// 窗口内完整可见的定稿段不冻结（保持可重排/可折叠）；跨越顶端的段
/// 整段冻结——否则其隐藏部分既不在屏上也不在 scrollback，无处翻看。
fn pick_flush_mark(
    marks: &[SettledMark],
    tail_start: usize,
    win_start: usize,
) -> Option<SettledMark> {
    let mut chosen = None;
    let mut prev_end = tail_start;
    for mark in marks {
        if mark.row_end > tail_start && prev_end.max(tail_start) < win_start {
            chosen = Some(*mark);
        }
        prev_end = mark.row_end;
    }
    chosen
}

/// 该行是否为图片块首行（块内续行返回 false；块边界按 url 识别）。
fn image_block_head(rows: &[Row], i: usize) -> bool {
    let Some(img) = &rows[i].line.image else {
        return false;
    };
    rows.get(i.wrapping_sub(1))
        .is_none_or(|prev| prev.line.image.as_ref().map(|p| &p.url) != Some(&img.url))
}

/// 按键分发。inline 模式下 ctrl+o 是展开/闭合切换（CC 非全屏语义），
/// 两个方向都不改已打印的 scrollback：展开 = 整卷重放冻结进 scrollback
/// （终端上滑翻看）；闭合 = 折回聚合态后走 resize 同款收拢（清屏重画 +
/// 回灌）。其余按键（含 Ctrl+C 的中断/清空/退出三态）全部交给
/// [`Chat`]，退出由 `chat.exit` 表达。
fn dispatch_key(chat: &mut Chat, key: KeyEvent, inline: bool) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    if inline {
        if key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if chat.transcript_fully_expanded() {
                if chat.collapse_transcript() {
                    // 撤销尚未渲染的重放（连按两次 = 净效果闭合），
                    // 清可见屏、按折叠后的高度回灌重画——屏上的展开
                    // 重放行只留在 scrollback。
                    chat.dump_transcript = false;
                    chat.force_redraw = true;
                    let chrome_len = chrome_rows(chat, chat.width, false).rows.len();
                    let budget = chat
                        .height
                        .saturating_sub(2)
                        .saturating_sub(chrome_len);
                    chat.rehydrate(chat.width, budget);
                }
            } else {
                chat.expand_transcript();
            }
            return;
        }
        if chat.ask_key(key.code) {
            return;
        }
    }
    chat.on_key(key.code, key.modifiers);
}

/// 文档重建（尺寸变化或状态脏）。viewport = 终端高度 - chrome 行数，
/// 与实际组装同源。
fn rebuild(chat: &mut Chat, size: Size, fullscreen: bool) {
    let width = size.width as usize;
    let height = size.height as usize;
    if chat.width != width || chat.height != height {
        chat.width = width;
        chat.height = height;
        chat.dirty = true;
    }
    let chrome = chrome_rows(chat, width, fullscreen).rows.len();
    let viewport = height.saturating_sub(chrome).max(1);
    if !chat.dirty && chat.viewport_height == viewport {
        return;
    }
    chat.viewport_height = viewport;
    if chat.dirty {
        chat.dirty = false;
        chat.reconcile_scroll(viewport);
        chat.build_rows(width);
    }
}

/// inline 宿主：定稿行一次性进 scrollback，只有底部视口重绘。
///
/// 宿主类型写死在这里（而不是对 `Backend` 泛型）：驱动对后端的约束比
/// `Backend` 更紧（它要能直接写字节），泛型化只会在集成时炸开。
pub async fn run_inline(
    mut chat: Chat,
    mut expand_rx: tokio::sync::watch::Receiver<bool>,
    mut term: StdoutTerm,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(TICK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks: u64 = 0;
    let mut expand_open = true;
    let mut dirty = true;
    let mut pending_resize: Option<(Size, Instant)> = None;

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => {
                    dispatch_key(&mut chat, key, true);
                    dirty = true;
                }
                Some(Ok(Event::Paste(text))) => {
                    chat.on_paste(&text);
                    dirty = true;
                }
                Some(Ok(Event::Resize(width, height))) => {
                    // 防抖：连发的 resize 只记录最新值，静默后统一应用。
                    pending_resize = Some((Size::new(width, height), Instant::now()));
                }
                Some(Ok(_)) => {}
                // Reading events failed (or stdin closed): the session cannot
                // be driven any more.
                Some(Err(_)) | None => break,
            },
            _ = ticker.tick() => {
                if let Some((size, at)) = pending_resize
                    && at.elapsed() >= Duration::from_millis(RESIZE_QUIET_MS)
                {
                    pending_resize = None;
                    term.resize(size)?;
                    chat.width = size.width as usize;
                    chat.height = size.height as usize;
                    // 终端 reflow 发生在 resize 事件到达之前，旧画面折行
                    // 位移不可知（内容顶到屏底还会整屏上滚）——不猜几何：
                    // 清可见屏、从头按新宽重画整个窗口（走 Ctrl+L 通道）。
                    // 回灌把内容拉回填满屏幕，画面无损；旧几何拷贝留在
                    // scrollback（接受上滑时看到重复）。
                    chat.force_redraw = true;
                    let chrome_len =
                        chrome_rows(&chat, size.width as usize, false).rows.len();
                    let doc_budget = (size.height as usize)
                        .saturating_sub(2)
                        .saturating_sub(chrome_len);
                    chat.rehydrate(size.width as usize, doc_budget);
                    chat.dirty = true;
                    dirty = true;
                }
                if chat.needs_tick() {
                    chat.tick();
                    if chat.drain_all() {
                        ticks = 0;
                    }
                    // 任务区不可见时不查磁盘。
                    if ticks.is_multiple_of(TASKS_REFRESH_TICKS) && chat.tasks_visible {
                        chat.refresh_tasks();
                    }
                    ticks = ticks.wrapping_add(1);
                    dirty = true;
                } else if !dirty {
                    // 空闲：无动画、无待处理事件、无待绘制变更 → 零写入。
                    continue;
                }
            },
            changed = expand_rx.changed(), if expand_open => {
                if changed.is_err() {
                    expand_open = false;
                } else {
                    if *expand_rx.borrow() {
                        chat.tasks_visible = true;
                        chat.tasks_auto = true;
                    }
                    chat.refresh_tasks();
                    dirty = true;
                }
            },
        }

        // 实体视图（ctrl+g 选择回车）：交替屏模态接管，返回后经 resize
        // 通道确定性重画（清屏 + 回灌，不猜交替屏恢复是否可靠）。
        if let Some(open) = chat.open_entity.take() {
            crate::tui::entity::run_entity_modal(&mut chat, &mut events, open, false).await?;
            if let Ok((w, h)) = crossterm::terminal::size() {
                pending_resize = Some((Size::new(w, h), Instant::now()));
            } else {
                chat.force_redraw = true;
            }
            chat.dirty = true;
            dirty = true;
        }

        // resize 风暴静默前不渲染（终端几何已变，旧宽的画面只添乱）；
        // 事件照常处理，静默后一帧补齐。
        if pending_resize.is_some() {
            if chat.exit {
                break;
            }
            continue;
        }

        // 退出前仍画完这一帧：最后一屏留在终端里（inline 退出不清屏）。
        if !dirty {
            if chat.exit {
                break;
            }
            continue;
        }
        dirty = false;

        // ctrl+l：清屏重画（花屏恢复）。
        if chat.force_redraw {
            chat.force_redraw = false;
            term.clear_visible()?;
        }

        let size = term.size();
        rebuild(&mut chat, size, false);

        // 懒落盘（与绘制合成 `term.frame` 单批次）：只冻结「起始行已越
        // 过窗口顶端」的定稿段——窗口内完整可见的定稿段留在活文档里随
        // 时可重排。视口收缩腾出的行进 gap 银行，冻结行随即写进这些行，
        // settle 迁移不闪、不留空白带。游标按段推进——哪怕该段只有图片
        // 续行（不产出条目），也必须推进，否则下一帧会重复画它。
        let mut items = Vec::new();
        if std::mem::take(&mut chat.dump_transcript) {
            // ctrl+o 整卷重放：游标已回卷、文档已从欢迎卡全量重建（全部
            // 展开），定稿部分一次冻结进 scrollback——用户上滑翻看全貌，
            // 动态尾部照常留在视口。
            if let Some(mark) = chat.doc.settled_marks.last().copied() {
                items = flush_items(&chat, size.width as usize, mark.row_end);
                chat.advance_flushed_upto(mark);
            }
        } else {
            let chrome_len = chrome_rows(&chat, size.width as usize, false).rows.len();
            // 窗口按「持久内容」算：瞬态 slash 输出（TTL 后消失）把窗口
            // 挤小不构成冻结活内容的理由——它只是暂时盖住，不是驱逐。
            let persistent = chat
                .doc
                .rows
                .len()
                .saturating_sub(chat.doc.transient_rows);
            let (win_start, _) = tail_window(
                persistent,
                chat.tail_start,
                chrome_len,
                size.height as usize,
            );
            if let Some(mark) =
                pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start)
            {
                items = flush_items(&chat, size.width as usize, mark.row_end);
                chat.advance_flushed_upto(mark);
            }
        }

        let frame = Frame::assemble(&chat, size);
        let height = u16::try_from(frame.rows.len()).unwrap_or(u16::MAX).max(1);
        let fg = chat.theme.text;
        term.frame(
            items,
            height,
            |buf| {
                let area = buf.area;
                view::render_rows(&frame.rows, fg, buf, area);
            },
            frame.cursor,
        )?;
        if chat.exit {
            break;
        }
    }

    term.finish()?;
    Ok(())
}

/// 全屏宿主：整篇文档 + app 内滚动 + 鼠标点击折叠，输入区吸底。
pub async fn run_fullscreen(
    mut chat: Chat,
    mut expand_rx: tokio::sync::watch::Receiver<bool>,
    mut terminal: FullscreenHost,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(TICK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks: u64 = 0;
    let mut expand_open = true;
    let mut dirty = true;

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => {
                    dispatch_key(&mut chat, key, false);
                    dirty = true;
                }
                Some(Ok(Event::Paste(text))) => {
                    chat.on_paste(&text);
                    dirty = true;
                }
                Some(Ok(Event::Mouse(mouse))) => {
                    if mouse_event(&mut chat, mouse) {
                        dirty = true;
                    }
                }
                Some(Ok(Event::Resize(_, _))) => {
                    chat.dirty = true;
                    dirty = true;
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
            _ = ticker.tick() => {
                if chat.needs_tick() {
                    chat.tick();
                    if chat.drain_all() {
                        ticks = 0;
                    }
                    if ticks.is_multiple_of(TASKS_REFRESH_TICKS) && chat.tasks_visible {
                        chat.refresh_tasks();
                    }
                    ticks = ticks.wrapping_add(1);
                    dirty = true;
                } else if !dirty {
                    continue;
                }
            },
            changed = expand_rx.changed(), if expand_open => {
                if changed.is_err() {
                    expand_open = false;
                } else {
                    if *expand_rx.borrow() {
                        chat.tasks_visible = true;
                        chat.tasks_auto = true;
                    }
                    chat.refresh_tasks();
                    dirty = true;
                }
            },
        }

        // 实体视图：已在交替屏内，模态直接接管画布，返回后整屏重画。
        if let Some(open) = chat.open_entity.take() {
            crate::tui::entity::run_entity_modal(&mut chat, &mut events, open, true).await?;
            chat.force_redraw = true;
            chat.dirty = true;
            dirty = true;
        }

        if !dirty {
            if chat.exit {
                break;
            }
            continue;
        }
        dirty = false;

        // ctrl+l：整屏重画（花屏恢复）。
        if chat.force_redraw {
            chat.force_redraw = false;
            terminal.clear()?;
        }

        let size = terminal.size()?;
        rebuild(&mut chat, size, true);
        let chrome = chrome_rows(&chat, size.width as usize, true);
        let viewport = (size.height as usize).saturating_sub(chrome.rows.len());
        let slice: Vec<Row> = chat
            .doc
            .rows
            .iter()
            .skip(chat.scroll)
            .take(viewport)
            .cloned()
            .collect();
        let fg = chat.theme.text;
        let chrome_start = (size.height as usize).saturating_sub(chrome.rows.len());
        let caret = {
            let (row, col) = caret_cell(&chat);
            caret_position(
                chrome_start + chrome.prompt_row + row,
                col + 2,
                0,
                size.height as usize,
                size.width as usize,
            )
        };
        terminal.draw(|frame| {
            let area = frame.area();
            let buf = frame.buffer_mut();
            view::render_rows(&slice, fg, buf, area);
            if let Ok(y) = u16::try_from(chrome_start) {
                view::render_rows(
                    &chrome.rows,
                    fg,
                    buf,
                    Rect::new(area.x, area.y.saturating_add(y), area.width, area.height),
                );
            }
            if let Some(position) = caret {
                frame.set_cursor_position(position);
            }
        })?;
        if chat.exit {
            break;
        }
    }
    Ok(())
}

/// 全屏鼠标：滚轮滚动、点击折叠/展开（点击行号 = 滚动位置 + 屏幕行）。
fn mouse_event(chat: &mut Chat, mouse: MouseEvent) -> bool {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            chat.auto_scroll = false;
            chat.scroll = chat.scroll.saturating_sub(WHEEL_ROWS);
            true
        }
        MouseEventKind::ScrollDown => {
            chat.auto_scroll = false;
            chat.scroll = chat.scroll.saturating_add(WHEEL_ROWS);
            true
        }
        MouseEventKind::Down(_) => {
            let doc_row = chat.scroll.saturating_add(mouse.row as usize);
            chat.doc_click(doc_row)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Session;
    use crate::tui::line::ImageRef;
    use std::sync::Arc;

    /// Text of an assembled row.
    fn row_text(row: &Row) -> String {
        row.line.plain_text()
    }

    /// A flushed scrollback line's text.
    fn history_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
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
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            last_task_reminder_turn: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: crate::agents::AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
        })
    }

    fn chat_at(width: usize, height: usize) -> Chat {
        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (asks_tx, asks_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut chat = Chat::new(
            test_session(),
            events_tx,
            events_rx,
            asks_tx,
            asks_rx,
            Theme::dark(),
            None,
        );
        chat.width = width;
        chat.height = height;
        chat
    }

    fn size(width: u16, height: u16) -> Size {
        Size::new(width, height)
    }

    #[test]
    fn tail_window_keeps_the_frame_below_terminal_height() {
        let total = 100usize;
        for height in 7..40usize {
            let chrome = 4usize;
            let (start, hidden) = tail_window(total, 0, chrome, height);
            let visible = total - start;
            let frame = visible + usize::from(hidden > 0) + chrome;
            assert!(frame < height, "height={height} frame={frame}");
            assert_eq!(hidden, total - visible, "省略数 = 未显示行数");
        }
        // 预算为零（chrome + 两行余量占满）：尾部一行不画，省略数不计。
        assert_eq!(tail_window(100, 0, 4, 6), (100, 0));
        // 内容装得下时不省略，也不裁剪。
        assert_eq!(tail_window(3, 0, 4, 40), (0, 0));
        // 已落盘的前缀不在尾部窗口内。
        assert_eq!(tail_window(3, 2, 4, 40), (2, 0));
        // chrome 已占满：尾部为空（画不下就一行不画，仍不越界）。
        assert_eq!(tail_window(3, 0, 4, 4), (3, 0));
    }

    /// 帧高度 = 组装出来的行数，且恒 < 终端高度：不再有第二套 chrome
    /// 公式可以和实际组装对不上。
    #[test]
    fn frame_height_never_reaches_terminal_height() {
        let mut chat = chat_at(80, 24);
        chat.doc.rows = (0..200)
            .map(|i| Row::new(Line::plain(format!("r{i}"))))
            .collect();
        for height in 4..40u16 {
            chat.height = height as usize;
            let frame = Frame::assemble(&chat, size(80, height));
            assert!(
                frame.rows.len() < height as usize,
                "height={height} rows={}",
                frame.rows.len()
            );
        }
    }

    /// 极矮终端：chrome 自己就超预算时保留底部（输入框 + footer），
    /// 帧仍不越界。
    #[test]
    fn tiny_terminal_keeps_the_prompt_and_footer() {
        let mut chat = chat_at(60, 6);
        chat.busy = true;
        chat.push_warning("mcp 连接失败".to_string());
        let frame = Frame::assemble(&chat, size(60, 6));
        assert_eq!(frame.rows.len(), 4, "height-2 上限");
        let text: Vec<String> = frame.rows.iter().map(row_text).collect();
        // 丢的是最上面的行（状态行/警告行），输入框与 footer 留住。
        assert!(text.last().is_some_and(|l| l.contains("ctrl+o to expand")), "{text:?}");
        assert!(text.iter().any(|l| l.starts_with('╰')), "输入框下边框仍在: {text:?}");
        assert!(text.iter().any(|l| l.starts_with('╭')), "输入框上边框仍在: {text:?}");
    }

    /// chrome 的每一段都出现在组装结果里（旧 Chrome 结构的对表测试：
    /// 现在计数就是组装本身，只需验证每段确实产出行）。
    #[test]
    fn chrome_contains_every_section() {
        let mut chat = chat_at(100, 40);
        let base = chrome_rows(&chat, 100, false).rows.len();
        // 空闲：输入框 3 行（两条边框 + 一行占位提示）+ footer 1 行。
        assert_eq!(base, 4);

        chat.busy = true;
        chat.push_warning("mcp 连接失败".to_string());
        let (tx, _rx) = tokio::sync::oneshot::channel();
        chat.pending_ask = Some((
            crate::ui::PermissionRequest::new("t", "q", vec!["a".into()]),
            tx,
        ));
        chat.set_input("a\nb\nc");
        chat.help_visible = true;
        chat.queued.push("queued message".into());
        chat.notice = Some("Press ctrl-c again to exit");
        chat.search = Some(crate::tui::chat::HistorySearch::default());
        let rows = chrome_rows(&chat, 100, false).rows;
        let text: Vec<String> = rows.iter().map(row_text).collect();
        assert!(text.iter().any(|l| l.contains("esc to interrupt")), "状态行");
        assert!(text.iter().any(|l| l.contains("⚠ mcp 连接失败")), "警告行");
        assert!(text.iter().any(|l| l.contains("shift+tab")), "? 面板");
        assert!(text.iter().any(|l| l.contains("(reverse-i-search)")), "搜索行");
        assert!(text.iter().any(|l| l.contains("> queued message")), "队列行");
        assert!(text.iter().any(|l| l.contains("Press ctrl-c again")), "提示行");
        assert!(text.iter().any(|l| l.contains("Waiting for permission…")), "ask 行");
        assert_eq!(
            text.iter().filter(|l| l.starts_with('╭') || l.starts_with('╰')).count(),
            2,
            "输入框上下边框"
        );
        // 每一段都算进行数：漏一段就是帧高度失真。
        assert_eq!(
            rows.len(),
            1 + chat.task_lines().len()
                + 1
                + chat.help_lines().len()
                + (2 + chat.prompt_lines().len())
                + 1
                + chat.queue_lines().len()
                + suggestion_rows(
                    &chat.slash_suggestions,
                    chat.slash_selected,
                    chat.model_menu.as_ref(),
                    chat.think_menu.as_ref(),
                    &chat.theme,
                    100
                )
                .len()
                + 1
                + 1
                + 1
        );
    }

    /// 建议区的行数与内容天然同源（旧版两套分支规则曾经对不上，
    /// canvas 因此越界）。
    #[test]
    fn suggestion_rows_cover_every_menu_state() {
        use crate::tui::chat::{ModelMenuModels, SlashSuggestion, THINK_LEVELS, ThinkMenu};
        let theme = Theme::dark();
        let mut menu = ModelMenu {
            providers: vec!["default".into(), "openrouter".into()],
            provider_selected: 0,
            models: None,
        };
        assert_eq!(suggestion_rows(&[], 0, None, None, &theme, 80).len(), 0);
        assert_eq!(suggestion_rows(&[], 0, Some(&menu), None, &theme, 80).len(), 2);
        // loading / 空列表各占一行提示。
        menu.models = Some(ModelMenuModels {
            provider: "default".into(),
            models: Vec::new(),
            loading: true,
            selected: 0,
        });
        assert_eq!(suggestion_rows(&[], 0, Some(&menu), None, &theme, 80).len(), 1);
        menu.models = Some(ModelMenuModels {
            provider: "default".into(),
            models: Vec::new(),
            loading: false,
            selected: 0,
        });
        assert_eq!(suggestion_rows(&[], 0, Some(&menu), None, &theme, 80).len(), 1);
        // 二级模型列表按 5+5 上限截断。
        menu.models = Some(ModelMenuModels {
            provider: "default".into(),
            models: (0..30).map(|i| format!("m{i}")).collect(),
            loading: false,
            selected: 0,
        });
        assert_eq!(
            suggestion_rows(&[], 0, Some(&menu), None, &theme, 80).len(),
            crate::tui::chat::SLASH_SUGGESTIONS_MAX + 5
        );
        // `/think` 菜单：每档一行，选中行带 ❯ 标记；模型菜单优先。
        let think = ThinkMenu { selected: 1 };
        let think_rows = suggestion_rows(&[], 0, None, Some(&think), &theme, 80);
        assert_eq!(think_rows.len(), THINK_LEVELS.len());
        assert!(
            row_text(&think_rows[1]).starts_with("❯ low"),
            "{}",
            row_text(&think_rows[1])
        );
        assert_eq!(
            suggestion_rows(&[], 0, Some(&menu), Some(&think), &theme, 80).len(),
            crate::tui::chat::SLASH_SUGGESTIONS_MAX + 5,
            "模型菜单优先于 think 菜单"
        );
        // slash 建议优先于菜单。
        let slash = vec![SlashSuggestion {
            name: "help".into(),
            description: "显示可用命令".into(),
        }];
        let rows = suggestion_rows(&slash, 0, Some(&menu), None, &theme, 80);
        assert_eq!(rows.len(), 1);
        assert!(row_text(&rows[0]).starts_with("❯ /help"), "{}", row_text(&rows[0]));
        // 每行都按宽度截断（超宽行会被终端折行，帧高度随即失真）。
        for width in 10..80usize {
            for row in suggestion_rows(&slash, 0, Some(&menu), None, &theme, width) {
                assert!(text_width(&row_text(&row)) <= width, "width={width}");
            }
            for row in suggestion_rows(&[], 0, None, Some(&think), &theme, width) {
                assert!(text_width(&row_text(&row)) <= width, "width={width}");
            }
        }
    }

    /// 状态行文案（CC ActivityIndicator）。
    #[test]
    fn status_row_renders_busy_verb() {
        let theme = Theme::dark();
        let status = crate::tui::chat::RunningStatus {
            verb: "Working".to_string(),
            elapsed: 12.5,
            tokens: 0,
        };
        let text = row_text(&status_row(&status, '✻', &theme));
        assert!(text.contains("✻ Working… (esc to interrupt · 13s)"), "{text}");
        assert!(!text.contains("tokens"), "0 token 省略该段: {text}");

        let status = crate::tui::chat::RunningStatus {
            verb: "$ cargo test".to_string(),
            elapsed: 3.2,
            tokens: 1200,
        };
        let text = row_text(&status_row(&status, '✽', &theme));
        assert!(
            text.contains("✽ $ cargo test… (esc to interrupt · 3s · ↓ 1200 tokens)"),
            "{text}"
        );
    }

    /// footer：徽标 + 提示（左）、模型名（右），bash 模式多一条 shell 提示。
    #[test]
    fn footer_shows_hints_and_model() {
        let mut chat = chat_at(80, 24);
        let text = row_text(&footer_row(&chat, 80));
        assert!(text.contains("? for shortcuts · ctrl+o to expand"), "{text}");
        assert!(text.contains("test-model"), "{text}");
        assert!(!text.contains("plan mode"), "default 模式无徽标: {text}");
        // 左右各留 2 列（CC footer padding）：模型名右端落在 width-2。
        assert_eq!(text_width(&text), 78, "模型名右对齐到 width-2");

        chat.busy = true;
        let text = row_text(&footer_row(&chat, 80));
        assert!(!text.contains("? for shortcuts"), "busy 只留 expand 提示: {text}");
        assert!(text.contains("ctrl+o to expand"), "{text}");

        chat.busy = false;
        chat.bash_mode = true;
        chat.permission_mode = PermissionMode::Plan;
        let text = row_text(&footer_row(&chat, 80));
        assert!(text.contains("⏸ plan mode on ·"), "{text}");
        assert!(text.contains("! for shell mode"), "{text}");
    }

    /// 输入框：前缀 + `▋` 假光标，真实光标落在同一格。
    #[test]
    fn prompt_rows_and_caret_agree() {
        let mut chat = chat_at(80, 24);
        chat.set_input("hi");
        let rows = prompt_rows(&chat, 80);
        assert_eq!(rows.len(), 3, "上下边框 + 一行输入");
        assert_eq!(row_text(&rows[1]), "❯ hi▋");
        assert_eq!(caret_cell(&chat), (0, 2), "光标在 ▋ 处");

        chat.set_input("");
        assert_eq!(caret_cell(&chat), (0, 0));
        assert!(row_text(&prompt_rows(&chat, 80)[1]).starts_with("❯ ▋"));

        // 多行输入：光标行随输入行走。
        chat.set_input("a\nb\nc");
        let rows = prompt_rows(&chat, 80);
        assert_eq!(rows.len(), 5);
        assert_eq!(caret_cell(&chat), (2, 1));

        chat.bash_mode = true;
        chat.set_input("ls");
        let rows = prompt_rows(&chat, 80);
        assert_eq!(row_text(&rows[1]), "! ls▋");
        // bash 模式换边框色（CC bashBorder）。
        assert_eq!(
            rows[0].line.segs[0].style.fg,
            Some(chat.theme.bash_border),
            "边框换色"
        );
    }

    /// 帧里的光标落在输入行的 `▋` 上（组装位移之后仍然对齐）。
    #[test]
    fn frame_cursor_points_at_the_caret() {
        let mut chat = chat_at(80, 24);
        chat.set_input("hello");
        chat.doc.rows = (0..5).map(|i| Row::new(Line::plain(format!("r{i}")))).collect();
        let frame = Frame::assemble(&chat, size(80, 24));
        let (x, y) = frame.cursor.expect("caret visible");
        assert_eq!(x, 7, "❯ + hello");
        let row = row_text(&frame.rows[y as usize]);
        assert_eq!(row, "❯ hello▋");
    }

    /// 落盘：定稿前缀转成 scrollback 条目，气泡行补满终端宽。
    #[test]
    fn flush_items_convert_settled_rows() {
        let mut chat = chat_at(40, 24);
        chat.doc.rows = vec![
            Row::new(Line::plain("first")),
            Row::bubble(Line::plain("❯ hi"), chat.theme.user_message_bg),
            Row::new(Line::plain("tail")),
        ];
        chat.doc.settled = 2;
        let items = flush_items(&chat, 40, chat.doc.settled);
        assert_eq!(items.len(), 2, "只落定稿前缀");
        let HistoryItem::Line(first) = &items[0] else {
            panic!("text row");
        };
        assert_eq!(history_text(first), "first");
        let HistoryItem::Line(bubble) = &items[1] else {
            panic!("bubble row");
        };
        assert_eq!(text_width(&history_text(bubble)), 40, "气泡满行");
    }

    /// 图片块：首行发字节（占 rows 行），续行跳过；无能力时退回占位文本。
    #[test]
    fn flush_items_emit_one_payload_per_image_block() {
        let mut chat = chat_at(40, 24);
        let img = |url: &str| Line {
            segs: Vec::new(),
            image: Some(ImageRef { url: url.into(), cols: 4, rows: 2 }),
        };
        chat.doc.rows = vec![
            Row::new(img("a.png")),
            Row::new(img("a.png")),
            Row::new(Line::plain("text")),
            Row::new(img("b.png")),
        ];
        chat.doc.settled = 4;
        // 无能力/无缓存：块首行退回 `#[image]` 占位，续行不出行。
        let items = flush_items(&chat, 40, chat.doc.settled);
        assert_eq!(items.len(), 3);
        let HistoryItem::Line(head) = &items[0] else {
            panic!("placeholder row");
        };
        assert_eq!(history_text(head), view::IMAGE_PLACEHOLDER);

        // 有能力 + 已加载：一块一份字节，行数 = 图片行数。
        chat.image_cap = Some(crate::tui::gfx::ImageCap::default_cells());
        chat.images.insert(
            "a.png".into(),
            std::sync::Arc::new(crate::tui::gfx::ImageMeta {
                cols: 4,
                rows: 2,
                bytes: b"png".to_vec(),
            }),
        );
        let items = flush_items(&chat, 40, chat.doc.settled);
        assert_eq!(items.len(), 3, "块内续行不重复落盘");
        match &items[0] {
            HistoryItem::Raw { bytes, rows } => {
                assert_eq!(*rows, 2, "占两行");
                assert!(!bytes.is_empty());
            }
            HistoryItem::Line(_) => panic!("image head should be raw bytes"),
        }
    }

    /// 块首/续行判定（图片块只发一次字节）。
    #[test]
    fn image_block_head_detects_block_boundaries() {
        let img = |url: &str| Line {
            segs: Vec::new(),
            image: Some(ImageRef { url: url.to_string(), cols: 10, rows: 3 }),
        };
        let rows = vec![
            Row::new(img("a.png")),
            Row::new(img("a.png")),
            Row::new(img("a.png")),
            Row::new(Line::plain("x")),
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

    /// inline 的核心不变量：定稿内容落盘一次，之后视口里只剩尾部 + chrome。
    #[test]
    fn flushed_rows_leave_the_viewport() {
        let mut chat = chat_at(80, 24);
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        let text: Vec<String> = Frame::assemble(&chat, size(80, 24))
            .rows
            .iter()
            .map(row_text)
            .collect();
        assert!(text.iter().any(|l| l.contains("Welcome back")), "首帧含欢迎卡: {text:?}");

        let items = flush_items(&chat, 80, chat.doc.settled);
        assert!(
            items.iter().any(|item| match item {
                HistoryItem::Line(line) => history_text(line).contains("Welcome back"),
                HistoryItem::Raw { .. } => false,
            }),
            "欢迎卡进 scrollback"
        );
        chat.advance_flushed();

        let text: Vec<String> = Frame::assemble(&chat, size(80, 24))
            .rows
            .iter()
            .map(row_text)
            .collect();
        assert!(
            !text.iter().any(|l| l.contains("Welcome back")),
            "落盘之后不再重画: {text:?}"
        );
        assert!(text.iter().any(|l| l.contains("? for shortcuts")), "chrome 仍在");
    }

    /// 落盘游标按消息段计：宽度变化让行号全变，也不会重复打印。
    #[test]
    fn flush_cursor_survives_a_width_change() {
        let mut chat = chat_at(80, 24);
        chat.messages.push(crate::tui::chat::UiMessage {
            role: crate::tui::chat::Role::User,
            text: "一条足够长的用户消息，宽度变化后折行数会变".repeat(2),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        let first = flush_items(&chat, 80, chat.doc.settled);
        assert!(!first.is_empty(), "首轮落盘欢迎卡 + 消息");
        chat.advance_flushed();
        // 同宽度再来一轮：没有新定稿内容 → 零条目。
        assert!(
            flush_items(&chat, 80, chat.doc.settled).is_empty(),
            "不重复落盘"
        );
        // 变窄重建：段游标不变，仍然没有新内容要落盘。
        chat.dirty = true;
        rebuild(&mut chat, size(40, 24), false);
        assert!(
            flush_items(&chat, 40, chat.doc.settled).is_empty(),
            "宽度变化不会让已落盘的段再打印一次"
        );
    }

    /// inline 的 ctrl+o：整卷重放——落盘游标回卷 + 置重放标志，重放帧
    /// 把全部定稿段冻结进 scrollback，视口只剩动态尾部与 chrome。
    #[test]
    fn ctrl_o_replays_the_full_transcript_inline() {
        let mut chat = chat_at(80, 24);
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        // 空会话且全部在屏 → no-op：不插字符、不重放。
        chat.set_input("hi");
        dispatch_key(&mut chat, key(KeyCode::Char('o'), KeyModifiers::CONTROL), true);
        assert_eq!(chat.input, "hi", "ctrl+o 未插入字符");
        assert!(!chat.dump_transcript, "屏上已是全貌，无需重放");

        // Esc 一律放行（菜单退出在 on_key 里）。
        chat.set_input("/model");
        chat.submit();
        assert!(chat.model_menu.is_some(), "菜单已打开");
        dispatch_key(&mut chat, key(KeyCode::Esc, KeyModifiers::empty()), true);
        assert!(chat.model_menu.is_none(), "Esc 经 gate 退出菜单");

        // 消息已落盘 → ctrl+o 请求重放；模拟重放帧：重建全量文档、
        // 整卷冻结到最后一个检查点。
        chat.messages.push(crate::tui::chat::UiMessage {
            role: crate::tui::chat::Role::Assistant,
            text: "reply".into(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.build_rows(80);
        chat.advance_flushed();
        dispatch_key(&mut chat, key(KeyCode::Char('o'), KeyModifiers::CONTROL), true);
        assert!(chat.dump_transcript, "已落盘内容 → 重放");
        assert!(chat.force_redraw, "重放帧先清可见屏（置顶）");
        assert!(chat.dirty, "重放帧前必然重建");
        chat.dirty = false;
        chat.build_rows(80);
        let mark = chat.doc.settled_marks.last().copied().expect("全量文档有检查点");
        let items = flush_items(&chat, 80, mark.row_end);
        let texts: Vec<String> = items
            .iter()
            .filter_map(|item| match item {
                HistoryItem::Line(line) => Some(history_text(line)),
                HistoryItem::Raw { .. } => None,
            })
            .collect();
        assert!(
            texts.iter().any(|l| l.contains("Welcome")),
            "重放从欢迎卡开始: {texts:?}"
        );
        assert!(
            texts.iter().any(|l| l.contains("reply")),
            "重放含已落盘消息: {texts:?}"
        );
        chat.advance_flushed_upto(mark);
        chat.build_rows(80);
        assert!(chat.doc.rows.is_empty(), "重放后活文档只剩动态尾部");
    }

    /// Release 事件不重复触发（终端上报增强键盘时会有）。
    #[test]
    fn key_release_is_ignored() {
        let mut chat = chat_at(80, 24);
        let mut key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty());
        key.kind = KeyEventKind::Release;
        dispatch_key(&mut chat, key, true);
        assert!(chat.input.is_empty());
    }

    /// 滚轮与点击（全屏）。
    #[test]
    fn mouse_scrolls_and_clicks() {
        let mut chat = chat_at(80, 24);
        chat.scroll = 10;
        let wheel = |kind| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        };
        assert!(mouse_event(&mut chat, wheel(MouseEventKind::ScrollUp)));
        assert_eq!(chat.scroll, 7);
        assert!(!chat.auto_scroll);
        assert!(mouse_event(&mut chat, wheel(MouseEventKind::ScrollDown)));
        assert_eq!(chat.scroll, 10);
    }

    /// 懒落盘：窗口装得下时不冻结；越过窗口顶的段（含跨越顶端的）整段冻结。
    #[test]
    fn pick_flush_mark_freezes_only_segments_past_the_window_top() {
        let marks = vec![
            SettledMark { row_end: 5, segments: 1 },
            SettledMark { row_end: 20, segments: 2 },
        ];
        // 全部可见（窗口从 0 开始）：什么都不冻结。
        assert_eq!(pick_flush_mark(&marks, 0, 0), None);
        // 窗口顶在第 3 行：段 1（0..5）跨越顶端 → 冻结到 5；段 2 从 5 起
        // 完整可见 → 保持活性。
        assert_eq!(pick_flush_mark(&marks, 0, 3), Some(marks[0]));
        // 窗口顶在第 10 行：段 2（5..20）也跨越了 → 一并冻结。
        assert_eq!(pick_flush_mark(&marks, 0, 10), Some(marks[1]));
        // 已冻结到 5 之后窗口未再上移：不重复选已消费的检查点。
        assert_eq!(pick_flush_mark(&marks, 5, 5), None);
    }

    /// 定稿内容在窗口内保持活性：小文档一帧都不冻结，宽度变化随重建重排。
    #[test]
    fn settled_rows_stay_live_while_they_fit() {
        let mut chat = chat_at(80, 24);
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert!(!chat.doc.settled_marks.is_empty(), "欢迎卡有定稿检查点");
        let chrome_len = chrome_rows(&chat, 80, false).rows.len();
        let (win_start, _) =
            tail_window(chat.doc.rows.len(), chat.tail_start, chrome_len, 24);
        assert_eq!(
            pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start),
            None,
            "装得下就不冻结——欢迎卡留在活文档里可重排"
        );
    }

    /// 瞬态 slash 输出（/resume 列表等）挤小窗口，不得因此冻结活内容。
    #[test]
    fn transient_slash_output_does_not_freeze_live_rows() {
        let mut chat = chat_at(80, 24);
        chat.slash_lines = (0..40).map(|i| format!("session-{i}")).collect();
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert_eq!(chat.doc.transient_rows, 40);
        let chrome_len = chrome_rows(&chat, 80, false).rows.len();
        let total = chat.doc.rows.len();

        // 回归防线：按全量文档算窗口，欢迎卡会被误判为越过顶端。
        let (naive_start, _) = tail_window(total, chat.tail_start, chrome_len, 24);
        assert!(
            pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, naive_start).is_some(),
            "前提成立：瞬态行确实把窗口挤过了欢迎卡"
        );

        // 生产路径剔除瞬态行：欢迎卡保持活性。
        let persistent = total - chat.doc.transient_rows;
        let (win_start, _) = tail_window(persistent, chat.tail_start, chrome_len, 24);
        assert_eq!(
            pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start),
            None,
            "瞬态列表只是暂时盖住内容，不是驱逐"
        );
    }

    /// 回灌：容量变大时取回已落盘的段重新渲染；超出预算即回退。
    #[test]
    fn rehydrate_refills_the_window_after_capacity_growth() {
        let mut chat = chat_at(80, 24);
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        let welcome_rows = chat.doc.rows.len();
        chat.advance_flushed();
        assert_eq!(chat.flushed_segments, 1, "欢迎卡已落盘");
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert!(chat.doc.rows.is_empty(), "落盘后活文档为空");

        // 预算充足：取回欢迎卡（scrollback 里的旧拷贝由用户上滑时接受重复）。
        chat.rehydrate(80, 24);
        assert_eq!(chat.flushed_segments, 0, "容量够就回灌");
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert_eq!(chat.doc.rows.len(), welcome_rows, "欢迎卡回到活文档");

        // 预算不足：回灌会超出 → 回退，保持已落盘状态。
        chat.advance_flushed();
        chat.rehydrate(80, welcome_rows.saturating_sub(1));
        assert_eq!(chat.flushed_segments, 1, "装不下就不取回");
    }
}
