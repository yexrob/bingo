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
use ratatui::layout::Size;
use ratatui::style::Color;

use crate::permission::PermissionMode;
use crate::tui::chat::{
    Chat, ModelMenu, ProviderMenu, ResumeMenu, Row, SettledMark, SlashSuggestion, ThemeMenu,
    ThinkMenu, model_footer_label,
};

/// 当前激活的选择器菜单（互斥：任一时刻至多一个；渲染按此分组读壳层）。
#[derive(Clone, Copy, Default)]
pub(crate) struct Menus<'a> {
    pub model: Option<&'a ModelMenu>,
    pub think: Option<&'a ThinkMenu>,
    pub theme: Option<&'a ThemeMenu>,
    pub resume: Option<&'a ResumeMenu>,
    pub provider: Option<&'a ProviderMenu>,
}
use crate::tui::gfx;
use crate::tui::line::{Line, SegStyle, text_width};
use crate::tui::term::{HistoryItem, StdoutTerm};
use crate::tui::theme::Theme;
use crate::tui::view;

/// Per-frame tick interval (spinner/thinking timing).
const TICK_MS: u64 = 33;
/// Disk-snapshot refresh interval for the task list (in ticks).
const TASKS_REFRESH_TICKS: u64 = 15;
/// Rows scrolled per mouse wheel notch (fullscreen only).
const WHEEL_ROWS: usize = 3;
/// Drag-resizing is an event storm: stay quiet this long before applying the new size and repainting. Painting at
/// the old width during the storm only piles more mis-width rows on screen (terminal reflow folds them into shards).
const RESIZE_QUIET_MS: u64 = 120;

/// Fullscreen host: the ready-made ratatui Terminal.
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

/// Inline tail window: returns (start row, hidden row count). The budget is the terminal height minus
/// chrome and a two-row margin — at least 2 screen rows always remain above the viewport top, so the DECSTBM
/// scroll region (which needs two rows) is always legal (same origin as term.rs's viewport cap).
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
    // The omission hint takes a row of its own.
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

/// Running status row (ActivityIndicator):
/// `✻ {verb}… (esc to interrupt · {N}s · ↓ {tokens} tokens)`.
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

/// Permission-mode badge (`⏸ plan mode on`) + the `·` separator after it.
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

/// Footer: mode badge + shortcut byline (left), model badge (right).
///
/// Model badge: `{provider} · {model} · think {level}` — the provider prefix
/// is omitted when it is default (keeps it terse); think off omits the level (P1-D).
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
    // `/think` picker preview: while the menu is open the badge shows the browsed
    // level with a `▸` suffix (would-be state) in the accent colour; committed
    // badge has no suffix and stays dim.
    let (model, model_color) = if let Some(menu) = &chat.think_menu {
        let level = crate::tui::chat::THINK_LEVELS
            [menu.selected.min(crate::tui::chat::THINK_LEVELS.len() - 1)]
        .0;
        (format!("{model_name} · think {level} ▸"), theme.claude)
    } else {
        (
            model_footer_label(&model_name, thinking.as_deref()),
            theme.inactive,
        )
    };
    let provider = chat.session.runtime.provider.borrow().clone();
    let model = if provider == "default" {
        model
    } else {
        format!("{provider} · {model}")
    };

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
    // Right-align the model name (also leaving 2 columns on the right).
    let gap = width.saturating_sub(used + text_width(&model) + 2).max(1);
    line.push_styled(" ".repeat(gap), SegStyle::fg(theme.inactive));
    line.push_styled(model, SegStyle::fg(model_color));
    Row::new(line)
}

/// Suggestion area: slash suggestions first, then the `/model` menu, then the `/think` menu.
/// Row count and content share one source — they were once separate, causing chrome to underestimate and the canvas to overflow.
fn suggestion_rows(
    slash: &[SlashSuggestion],
    slash_selected: usize,
    menus: Menus<'_>,
    no_match: bool,
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
        let Some(menu) = menus.model else {
            // `/think` level selector (when the model menu is inactive).
            if let Some(think) = menus.think {
                // 薄壳 → 核心：行渲染与按键提示行统一委托 PickerModel（picker-model.md 提交 A）。
                let core = think.picker();
                let mut rows: Vec<Row> = (0..core.items.len())
                    .map(|i| core.row(i, width, theme))
                    .collect();
                rows.push(core.hint_row(crate::tui::chat::ThinkMenu::keys(), width, theme));
                return rows;
            }
            // `/theme` level selector（picker-model.md 提交 B）：同款薄壳渲染。
            if let Some(theme_menu) = menus.theme {
                let core = theme_menu.picker();
                let mut rows: Vec<Row> = (0..core.items.len())
                    .map(|i| core.row(i, width, theme))
                    .collect();
                rows.push(core.hint_row(crate::tui::chat::ThemeMenu::keys(), width, theme));
                return rows;
            }
            // `/provider` selector（picker-model.md 提交 D）：同款薄壳渲染。
            if let Some(provider_menu) = menus.provider {
                let core = provider_menu.picker();
                let mut rows: Vec<Row> = (0..core.items.len())
                    .map(|i| core.row(i, width, theme))
                    .collect();
                rows.push(core.hint_row(crate::tui::chat::ProviderMenu::keys(), width, theme));
                return rows;
            }
            // `/resume` session selector（picker-model.md 提交 C）：截断时追加说明行。
            if let Some(resume_menu) = menus.resume {
                let core = resume_menu.picker();
                let mut rows: Vec<Row> = (0..core.items.len())
                    .map(|i| core.row(i, width, theme))
                    .collect();
                rows.push(core.hint_row(crate::tui::chat::ResumeMenu::keys(), width, theme));
                if resume_menu.truncated {
                    rows.push(Row::new(Line::styled(
                        format!(
                            "  {}",
                            crate::tui::markdown::truncate(
                                "（仅显示最近 20 个会话）",
                                width.saturating_sub(2),
                            )
                        ),
                        SegStyle::fg(theme.inactive),
                    )));
                }
                return rows;
            }
            // G9: a bare `/`-query with zero matches gets one dim hint row.
            if no_match {
                return vec![Row::new(Line::styled(
                    "  （无匹配命令 · 输入 /help 查看可用命令）",
                    SegStyle::fg(theme.inactive),
                ))];
            }
            return Vec::new();
        };
        // `/model` two-level selector: level one `provider`（PickerModel 核心渲染，
        // picker-model.md 提交 E）、level two `model`（loading / empty list 各一行提示）。
        let Some(m) = &menu.models else {
            // 一级：● 标当前 provider + 数字直达提示行（Enter = 查看模型列表）。
            let core = menu.provider_picker();
            let mut rows: Vec<Row> = (0..core.items.len())
                .map(|i| core.row(i, width, theme))
                .collect();
            rows.push(Row::new(Line::styled(
                format!(
                    "  {}",
                    crate::tui::markdown::truncate(
                        "↑↓/1-9 选择 provider · Enter 查看模型 · Esc 返回",
                        width.saturating_sub(2),
                    )
                ),
                SegStyle::fg(theme.inactive),
            )));
            return rows;
        };
        let items: Vec<(String, bool)> = if m.loading {
            vec![(format!("… 正在拉取 {} 的模型列表", m.provider), true)]
        } else if m.models.is_empty() {
            vec![("（该端点未返回模型，Esc 退出）".to_string(), true)]
        } else {
            m.models
                .iter()
                .enumerate()
                .map(|(i, name)| (name.clone(), i == m.selected))
                .collect()
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
        .map(|s| s.name.chars().count() + usize::from(!s.hint.is_empty()) + s.hint.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    // Available description width = terminal width - padding(2) - "❯ "(2) - name column - separator(2).
    let desc_width = width.saturating_sub(2 + 2 + name_col + 2).max(8);
    slash
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let selected = i == slash_selected;
            let cmd = if s.hint.is_empty() {
                format!("/{}", s.name)
            } else {
                format!("/{} {}", s.name, s.hint)
            };
            let name_text = format!("{cmd:<name_col$}");
            let desc = crate::tui::markdown::truncate(&s.description, desc_width);
            let line = crate::tui::markdown::truncate(
                &format!("{}{name_text}  {desc}", if selected { "❯ " } else { "  " }),
                width.saturating_sub(2),
            );
            row(line, selected)
        })
        .collect()
}

/// Input box (top border + input rows + bottom border).
fn prompt_rows(chat: &Chat, width: usize) -> Vec<Row> {
    let theme = &chat.theme;
    let border_color = if chat.bash_mode {
        theme.bash_border
    } else {
        theme.prompt_border
    };
    let prompt_style = if chat.busy {
        theme.inactive
    } else {
        theme.text
    };
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
            if i == 0 {
                prefix.clone()
            } else {
                "  ".to_string()
            },
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

/// Caret position inside the input box (row offset, column) — same source as where
/// [`Chat::prompt_lines`] draws `▋`.
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

/// Every row outside the transcript, top to bottom. `fullscreen` only moves the suggestion area
/// (fullscreen: above the input; inline: below, aligned with slash output).
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
    // Bottom entity area (agent instances + channels; ctrl+g focuses the selector).
    for line in chat.entity_rows(width) {
        rows.push(Row::new(line));
    }

    let suggestions = suggestion_rows(
        &chat.slash_suggestions,
        chat.slash_selected,
        Menus {
            model: chat.model_menu.as_ref(),
            think: chat.think_menu.as_ref(),
            theme: chat.theme_menu.as_ref(),
            resume: chat.resume_menu.as_ref(),
            provider: chat.provider_menu.as_ref(),
        },
        chat.slash_no_match,
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

/// #18 full-flow full-screen error-state skeleton (AC-26/53, ui/ux #68 spec): error title +
/// stable code + description (what happened + what you can do) + primary action (retry/back) + exit hint.
/// Actions are bound at the key layer (chat.rs: Enter=retry, Esc=back on the full-screen state); this function only draws.
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
    /// Inline frame: dynamic tail (over budget → keep only the last rows + the omission hint) + chrome.
    /// The row count is the viewport height, so it is always ≤ terminal height - 2 (the DECSTBM region stays legal).
    /// #18: the full-flow error state (`last_error.level == Full`) covers the content area with a full-screen error,
    /// and the input caret is hidden (the user is on the error screen; the key layer handles primary actions).
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
        // #18 error row (Page/Field levels): generated from the structured `last_error`, highlighted in the error
        // color (A zone), appended at the end of the content area — no doc rebuild, no double display.
        if let Some(err) = &chat.last_error
            && err.level != crate::error::ErrorLevel::Full
        {
            rows.push(Row::new(Line::styled(
                format!("[error] code={} msg={}", err.code, err.msg),
                SegStyle::fg(chat.theme.error),
            )));
        }
        rows.extend(chrome.rows);

        // Last line of defense: chrome itself can exceed the budget (very short terminals),
        // in which case drop the top rows — the input box and footer are the part that must stay.
        // Budget = height − 2: same as term.rs's viewport cap (two rows left on top,
        // so the DECSTBM scroll region is always legal).
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

/// Caret cell: shown only if it still lands on screen after the frame top dropped `dropped` rows.
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

/// Newly settled rows → scrollback entries. The first row of an image block emits real kitty bytes (transfer +
/// placement + cursor advance); the sequence consumes the continuation rows, so they are skipped.
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

/// Lazy-flush pick: the furthest settled checkpoint whose segment's start row has crossed the window top.
/// Fully visible settled segments stay unfrozen (kept re-layoutable/collapsible); a segment crossing the top
/// freezes wholesale — otherwise its hidden part exists neither on screen nor in scrollback, with nowhere to look.
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

/// Whether this row is an image block's first row (continuation rows return false; boundaries are detected by url).
fn image_block_head(rows: &[Row], i: usize) -> bool {
    let Some(img) = &rows[i].line.image else {
        return false;
    };
    rows.get(i.wrapping_sub(1))
        .is_none_or(|prev| prev.line.image.as_ref().map(|p| &p.url) != Some(&img.url))
}

/// Key dispatch. In inline mode ctrl+o toggles expand/collapse (CC non-fullscreen semantics);
/// neither direction touches the already-printed scrollback: expand = replay the whole transcript and freeze it
/// into scrollback (readable by scrolling up); collapse = fold back to aggregates, then close up like resize (clear-redraw +
/// rehydration). All other keys (including Ctrl+C's interrupt/clear/quit three states) go to
/// [`Chat`]; quitting is expressed via `chat.exit`.
fn dispatch_key(chat: &mut Chat, key: KeyEvent, inline: bool) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    if inline {
        if key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if chat.transcript_fully_expanded() {
                if chat.collapse_transcript() {
                    // Cancel the not-yet-rendered replay (pressing twice = net effect of collapse),
                    // clear the visible screen and redraw by rehydrating to the collapsed height — the expanded
                    // replay rows on screen stay only in scrollback.
                    chat.dump_transcript = false;
                    chat.force_redraw = true;
                    let chrome_len = chrome_rows(chat, chat.width, false).rows.len();
                    let budget = chat.height.saturating_sub(2).saturating_sub(chrome_len);
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

/// Document rebuild (on size change or dirty state). viewport = terminal height - chrome rows,
/// from the same source as the actual assembly.
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

/// Inline host: settled rows go into scrollback in one go; only the bottom viewport is repainted.
///
/// The host type is hard-coded here (instead of being generic over `Backend`): the driver's constraint on the backend
/// is tighter than `Backend` (it must write raw bytes); generifying would only blow up at integration time.
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
                    // Debounce: rapid resizes only record the latest value, applied once things quiet down.
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
                    // Terminal reflow happens before the resize event arrives; the old frame's wrapped rows
                    // shift by an unknown amount (content can even scroll the whole screen) — do not guess geometry:
                    // clear the visible screen and redraw the whole window at the new width (via the Ctrl+L path).
                    // Rehydration pulls the content back to fill the screen losslessly; the old-geometry copies stay
                    // in scrollback (accept duplicates when scrolling up).
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
                    // Skip disk reads while the task area is hidden.
                    if ticks.is_multiple_of(TASKS_REFRESH_TICKS) && chat.tasks_visible {
                        chat.refresh_tasks();
                    }
                    ticks = ticks.wrapping_add(1);
                    dirty = true;
                } else if !dirty {
                    // Idle: no animation, no pending events, no pending draw changes → zero writes.
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

        // Entity view (ctrl+g then Enter): the alternate-screen modal takes over; afterwards, a deterministic
        // redraw goes through the resize channel (clear + rehydrate, without guessing whether alt-screen restore works).
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

        // Do not render before the resize storm quiets down (the terminal geometry has changed; old-width
        // frames only add noise); events are handled as usual and one frame catches up after the quiet.
        if pending_resize.is_some() {
            if chat.exit {
                break;
            }
            continue;
        }

        // Finish the current frame before quitting: the last screen stays in the terminal (inline exit does not clear).
        if !dirty {
            if chat.exit {
                break;
            }
            continue;
        }
        dirty = false;

        // ctrl+l: clear and repaint (recover from a garbled screen).
        if chat.force_redraw {
            chat.force_redraw = false;
            term.clear_visible()?;
        }

        let size = term.size();
        rebuild(&mut chat, size, false);

        // Lazy flush (composited with drawing into one `term.frame` batch): freeze only the settled segments
        // whose start row has crossed the window top — fully visible settled segments stay in the live doc
        // for re-layout at any time. Rows freed by a shrinking viewport go into the gap bank and frozen rows
        // are written into them right away, so settling migrates without flicker or blank bands. The cursor
        // advances per segment — even an image-only continuation segment (no items) must advance, or the next frame would redraw it.
        let mut items = Vec::new();
        if std::mem::take(&mut chat.dump_transcript) {
            // ctrl+o full replay: the cursor has rewound and the doc fully rebuilt from the welcome card (everything
            // expanded); the settled part freezes into scrollback in one go — the user scrolls up to see it all,
            // while the dynamic tail stays in the viewport as usual.
            if let Some(mark) = chat.doc.settled_marks.last().copied() {
                items = flush_items(&chat, size.width as usize, mark.row_end);
                chat.advance_flushed_upto(mark);
            }
        } else {
            let chrome_len = chrome_rows(&chat, size.width as usize, false).rows.len();
            // The window counts "persistent content": transient slash output (gone after TTL) squeezing the window
            // is no reason to freeze live content — it merely covers it temporarily, not evicts it.
            let persistent = chat.doc.rows.len().saturating_sub(chat.doc.transient_rows);
            let (win_start, _) = tail_window(
                persistent,
                chat.tail_start,
                chrome_len,
                size.height as usize,
            );
            if let Some(mark) = pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start)
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

/// Assembles the alternate-screen canvas. Unlike [`Frame::assemble`], normal content
/// fills the terminal and pins chrome to the bottom.
fn fullscreen_frame(chat: &Chat, size: Size) -> Frame {
    if let Some(err) = &chat.last_error
        && err.level == crate::error::ErrorLevel::Full
    {
        return Frame {
            rows: error_screen_rows(err, &chat.theme),
            cursor: None,
        };
    }

    let chrome = chrome_rows(chat, size.width as usize, true);
    let viewport = (size.height as usize).saturating_sub(chrome.rows.len());
    let mut rows: Vec<Row> = chat
        .doc
        .rows
        .iter()
        .skip(chat.scroll)
        .take(viewport)
        .cloned()
        .collect();
    let chrome_start = (size.height as usize).saturating_sub(chrome.rows.len());
    rows.resize_with(chrome_start, || Row::new(Line::plain("")));
    rows.extend(chrome.rows);
    let (row, col) = caret_cell(chat);
    let cursor = caret_position(
        chrome_start + chrome.prompt_row + row,
        col + 2,
        0,
        size.height as usize,
        size.width as usize,
    );
    Frame { rows, cursor }
}

/// Fullscreen host: the whole document + in-app scrolling + mouse-click folding, input area pinned to the bottom.
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

        // Entity view: already on the alternate screen, the modal takes over the canvas directly; full repaint after return.
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

        // ctrl+l: full repaint (recover from a garbled screen).
        if chat.force_redraw {
            chat.force_redraw = false;
            terminal.clear()?;
        }

        let size = terminal.size()?;
        rebuild(&mut chat, size, true);
        let frame = fullscreen_frame(&chat, size);
        let fg = chat.theme.text;
        terminal.draw(|terminal_frame| {
            let area = terminal_frame.area();
            let buf = terminal_frame.buffer_mut();
            view::render_rows(&frame.rows, fg, buf, area);
            if let Some(position) = frame.cursor {
                terminal_frame.set_cursor_position(position);
            }
        })?;
        if chat.exit {
            break;
        }
    }
    Ok(())
}

/// Fullscreen mouse: wheel scrolls, clicks fold/expand (clicked row number = scroll position + screen row).
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
            crate::tui::theme::ThemeSetting::Auto,
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
        // Zero budget (chrome + two-row margin fill it): no tail row is drawn; the hidden count is zero.
        assert_eq!(tail_window(100, 0, 4, 6), (100, 0));
        // When content fits, nothing is omitted or clipped.
        assert_eq!(tail_window(3, 0, 4, 40), (0, 0));
        // The flushed prefix is outside the tail window.
        assert_eq!(tail_window(3, 2, 4, 40), (2, 0));
        // Chrome fills everything: the tail is empty (nothing is drawn if it does not fit; still never overflows).
        assert_eq!(tail_window(3, 0, 4, 4), (3, 0));
    }

    /// Frame height = the assembled row count, always < terminal height: no second chrome
    /// formula can drift from the actual assembly.
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

    /// Very short terminals: when chrome itself exceeds the budget, keep the bottom (input + footer);
    /// the frame still never overflows.
    #[test]
    fn tiny_terminal_keeps_the_prompt_and_footer() {
        let mut chat = chat_at(60, 6);
        chat.busy = true;
        chat.push_warning("mcp 连接失败".to_string());
        let frame = Frame::assemble(&chat, size(60, 6));
        assert_eq!(frame.rows.len(), 4, "height-2 上限");
        let text: Vec<String> = frame.rows.iter().map(row_text).collect();
        // The dropped rows are the top ones (status/warning); the input and footer stay.
        assert!(
            text.last().is_some_and(|l| l.contains("ctrl+o to expand")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|l| l.starts_with('╰')),
            "输入框下边框仍在: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.starts_with('╭')),
            "输入框上边框仍在: {text:?}"
        );
    }

    /// Every chrome section appears in the assembled result (a table-check test for the old Chrome structure:
    /// now the count is the assembly itself, so just verify each section really produces rows).
    #[test]
    fn chrome_contains_every_section() {
        let mut chat = chat_at(100, 40);
        let base = chrome_rows(&chat, 100, false).rows.len();
        // Idle: input 3 rows (two borders + one placeholder row) + footer 1 row.
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
        chat.queued.push(crate::tui::chat::QueuedInput {
            text: "queued message".into(),
            is_slash: false,
        });
        chat.notice = Some("Press ctrl-c again to exit");
        chat.search = Some(crate::tui::chat::HistorySearch::default());
        let rows = chrome_rows(&chat, 100, false).rows;
        let text: Vec<String> = rows.iter().map(row_text).collect();
        assert!(
            text.iter().any(|l| l.contains("esc to interrupt")),
            "状态行"
        );
        assert!(text.iter().any(|l| l.contains("⚠ mcp 连接失败")), "警告行");
        assert!(text.iter().any(|l| l.contains("shift+tab")), "? 面板");
        assert!(
            text.iter().any(|l| l.contains("(reverse-i-search)")),
            "搜索行"
        );
        assert!(
            text.iter().any(|l| l.contains("> queued message")),
            "队列行"
        );
        assert!(
            text.iter().any(|l| l.contains("Press ctrl-c again")),
            "提示行"
        );
        assert!(
            text.iter().any(|l| l.contains("Waiting for permission…")),
            "ask 行"
        );
        assert_eq!(
            text.iter()
                .filter(|l| l.starts_with('╭') || l.starts_with('╰'))
                .count(),
            2,
            "输入框上下边框"
        );
        // Every section counts toward the row total: missing one means the frame height is wrong.
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
                    Menus {
                        model: chat.model_menu.as_ref(),
                        think: chat.think_menu.as_ref(),
                        theme: chat.theme_menu.as_ref(),
                        resume: chat.resume_menu.as_ref(),
                        provider: chat.provider_menu.as_ref(),
                    },
                    chat.slash_no_match,
                    &chat.theme,
                    100
                )
                .len()
                + 1
                + 1
                + 1
        );
    }

    /// The suggestion area's row count and content are naturally one source (the old two-branch rules once disagreed,
    /// overflowing the canvas).
    #[test]
    fn suggestion_rows_cover_every_menu_state() {
        use crate::tui::chat::{ModelMenuModels, SlashSuggestion, THINK_LEVELS, ThinkMenu};
        let theme = Theme::dark();
        let mut menu = ModelMenu {
            providers: vec!["default".into(), "openrouter".into()],
            provider_selected: 0,
            provider_current: Some(0),
            models: None,
        };
        assert_eq!(
            suggestion_rows(&[], 0, Menus::default(), false, &theme, 80).len(),
            0
        );
        // G9: no-match shows one dim hint row instead of an empty gap.
        let no_match = suggestion_rows(&[], 0, Menus::default(), true, &theme, 80);
        assert_eq!(no_match.len(), 1);
        assert!(
            row_text(&no_match[0]).contains("无匹配命令"),
            "{}",
            row_text(&no_match[0])
        );
        // Level one: 2 provider rows + 1 hint row（picker-model.md 提交 E）。
        assert_eq!(
            suggestion_rows(
                &[],
                0,
                Menus {
                    model: Some(&menu),
                    think: None,
                    theme: None,
                    resume: None,
                    provider: None
                },
                false,
                &theme,
                80
            )
            .len(),
            3
        );
        // Loading / empty list each take one hint row.
        menu.models = Some(ModelMenuModels {
            provider: "default".into(),
            models: Vec::new(),
            loading: true,
            selected: 0,
        });
        assert_eq!(
            suggestion_rows(
                &[],
                0,
                Menus {
                    model: Some(&menu),
                    think: None,
                    theme: None,
                    resume: None,
                    provider: None
                },
                false,
                &theme,
                80
            )
            .len(),
            1
        );
        menu.models = Some(ModelMenuModels {
            provider: "default".into(),
            models: Vec::new(),
            loading: false,
            selected: 0,
        });
        assert_eq!(
            suggestion_rows(
                &[],
                0,
                Menus {
                    model: Some(&menu),
                    think: None,
                    theme: None,
                    resume: None,
                    provider: None
                },
                false,
                &theme,
                80
            )
            .len(),
            1
        );
        // The level-two model list truncates at the 5+5 cap.
        menu.models = Some(ModelMenuModels {
            provider: "default".into(),
            models: (0..30).map(|i| format!("m{i}")).collect(),
            loading: false,
            selected: 0,
        });
        assert_eq!(
            suggestion_rows(
                &[],
                0,
                Menus {
                    model: Some(&menu),
                    think: None,
                    theme: None,
                    resume: None,
                    provider: None
                },
                false,
                &theme,
                80
            )
            .len(),
            crate::tui::chat::SLASH_SUGGESTIONS_MAX + 5
        );
        // `/think` menu: one row per level + one hint row; `●` marks the in-effect
        // level, `❯` the browse selection (two separate marks); the model menu takes priority.
        let think = ThinkMenu {
            selected: 1,
            current: 0,
        };
        let think_rows = suggestion_rows(
            &[],
            0,
            Menus {
                model: None,
                think: Some(&think),
                theme: None,
                resume: None,
                provider: None,
            },
            false,
            &theme,
            80,
        );
        assert_eq!(think_rows.len(), THINK_LEVELS.len() + 1, "6 档 + 提示行");
        assert!(
            row_text(&think_rows[0]).contains("● off"),
            "● 标当前生效档: {}",
            row_text(&think_rows[0])
        );
        assert!(
            row_text(&think_rows[1]).starts_with("  ❯"),
            "❯ 标浏览选中: {}",
            row_text(&think_rows[1])
        );
        assert!(
            row_text(&think_rows[1]).contains("low"),
            "选中行名: {}",
            row_text(&think_rows[1])
        );
        // Overlap: ❯ keeps the prefix slot, ● stays in front of the name.
        let overlap = ThinkMenu {
            selected: 3,
            current: 3,
        };
        let rows = suggestion_rows(
            &[],
            0,
            Menus {
                model: None,
                think: Some(&overlap),
                theme: None,
                resume: None,
                provider: None,
            },
            false,
            &theme,
            80,
        );
        assert!(
            row_text(&rows[3]).contains("❯ ● high"),
            "重叠行双标记: {}",
            row_text(&rows[3])
        );
        // Hint row (last, dim).
        let hint = row_text(think_rows.last().unwrap());
        assert!(hint.contains("Esc 取消"), "提示行: {hint}");
        assert_eq!(
            suggestion_rows(
                &[],
                0,
                Menus {
                    model: Some(&menu),
                    think: Some(&think),
                    theme: None,
                    resume: None,
                    provider: None
                },
                false,
                &theme,
                80
            )
            .len(),
            crate::tui::chat::SLASH_SUGGESTIONS_MAX + 5,
            "模型菜单优先于 think 菜单"
        );
        // Slash suggestions take priority over menus.
        let slash = vec![SlashSuggestion {
            name: "help".into(),
            hint: String::new(),
            description: "显示可用命令".into(),
        }];
        let rows = suggestion_rows(
            &slash,
            0,
            Menus {
                model: Some(&menu),
                think: None,
                theme: None,
                resume: None,
                provider: None,
            },
            false,
            &theme,
            80,
        );
        assert_eq!(rows.len(), 1);
        assert!(
            row_text(&rows[0]).starts_with("❯ /help"),
            "{}",
            row_text(&rows[0])
        );
        // A command with an argument hint renders name + hint in the name column.
        let with_hint = vec![SlashSuggestion {
            name: "think".into(),
            hint: "[off|low|medium|high|xhigh|max]".into(),
            description: "设置思考级别".into(),
        }];
        let rows = suggestion_rows(&with_hint, 0, Menus::default(), false, &theme, 80);
        assert_eq!(rows.len(), 1);
        assert!(
            row_text(&rows[0]).contains("/think [off|low|medium|high|xhigh|max]"),
            "{}",
            row_text(&rows[0])
        );
        // Every row truncates by width (overwide rows would be wrapped by the terminal, skewing the frame height).
        for width in 10..80usize {
            for row in suggestion_rows(
                &slash,
                0,
                Menus {
                    model: Some(&menu),
                    think: None,
                    theme: None,
                    resume: None,
                    provider: None,
                },
                false,
                &theme,
                width,
            ) {
                assert!(text_width(&row_text(&row)) <= width, "width={width}");
            }
            for row in suggestion_rows(
                &[],
                0,
                Menus {
                    model: None,
                    think: Some(&think),
                    theme: None,
                    resume: None,
                    provider: None,
                },
                false,
                &theme,
                width,
            ) {
                assert!(text_width(&row_text(&row)) <= width, "width={width}");
            }
        }
    }

    /// Status-row copy (CC ActivityIndicator).
    #[test]
    fn status_row_renders_busy_verb() {
        let theme = Theme::dark();
        let status = crate::tui::chat::RunningStatus {
            verb: "Working".to_string(),
            elapsed: 12.5,
            tokens: 0,
        };
        let text = row_text(&status_row(&status, '✻', &theme));
        assert!(
            text.contains("✻ Working… (esc to interrupt · 13s)"),
            "{text}"
        );
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

    /// Footer: badge + hints (left), model name (right); bash mode adds a shell hint.
    #[test]
    fn footer_shows_hints_and_model() {
        let mut chat = chat_at(80, 24);
        let text = row_text(&footer_row(&chat, 80));
        assert!(
            text.contains("? for shortcuts · ctrl+o to expand"),
            "{text}"
        );
        assert!(text.contains("test-model"), "{text}");
        assert!(!text.contains("plan mode"), "default 模式无徽标: {text}");
        // 2 columns of padding on each side (CC footer padding): the model name's right edge lands at width-2.
        assert_eq!(text_width(&text), 78, "模型名右对齐到 width-2");

        chat.busy = true;
        let text = row_text(&footer_row(&chat, 80));
        assert!(
            !text.contains("? for shortcuts"),
            "busy 只留 expand 提示: {text}"
        );
        assert!(text.contains("ctrl+o to expand"), "{text}");

        chat.busy = false;
        chat.bash_mode = true;
        chat.permission_mode = PermissionMode::Plan;
        let text = row_text(&footer_row(&chat, 80));
        assert!(text.contains("⏸ plan mode on ·"), "{text}");
        assert!(text.contains("! for shell mode"), "{text}");
    }

    /// Footer `/think` picker preview: while the menu is open the badge shows the
    /// browsed level with `▸`; committed badge has no suffix (Esc reverts via the
    /// same branch — the menu is gone).
    #[test]
    fn footer_previews_browsed_think_level() {
        let mut chat = chat_at(80, 24);
        let _ = chat
            .session
            .runtime
            .thinking_tx
            .send(Some("high".to_string()));
        let text = row_text(&footer_row(&chat, 80));
        assert!(text.contains("test-model · think high"), "{text}");
        assert!(!text.contains('▸'), "提交态无预览后缀: {text}");

        // Open the picker (preselects high); browse to xhigh → preview shows xhigh ▸.
        chat.input = "/think".to_string();
        chat.submit();
        let text = row_text(&footer_row(&chat, 80));
        assert!(text.contains("test-model · think high ▸"), "{text}");
        chat.on_key(
            ratatui::crossterm::event::KeyCode::Down,
            ratatui::crossterm::event::KeyModifiers::empty(),
        );
        let text = row_text(&footer_row(&chat, 80));
        assert!(
            text.contains("test-model · think xhigh ▸"),
            "预览跟随浏览: {text}"
        );
        // Esc reverts to the committed badge (no suffix).
        chat.on_key(
            ratatui::crossterm::event::KeyCode::Esc,
            ratatui::crossterm::event::KeyModifiers::empty(),
        );
        let text = row_text(&footer_row(&chat, 80));
        assert!(text.contains("test-model · think high"), "{text}");
        assert!(!text.contains('▸'), "Esc 后还原: {text}");
    }

    /// Input box: prefix + `▋` fake caret; the real caret lands on the same cell.
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

        // Multi-line input: the caret row follows the input row.
        chat.set_input("a\nb\nc");
        let rows = prompt_rows(&chat, 80);
        assert_eq!(rows.len(), 5);
        assert_eq!(caret_cell(&chat), (2, 1));

        chat.bash_mode = true;
        chat.set_input("ls");
        let rows = prompt_rows(&chat, 80);
        assert_eq!(row_text(&rows[1]), "! ls▋");
        // bash mode swaps the border color (CC bashBorder).
        assert_eq!(
            rows[0].line.segs[0].style.fg,
            Some(chat.theme.bash_border),
            "边框换色"
        );
    }

    /// The frame caret lands on the input row's `▋` (still aligned after the assembly offsets).
    #[test]
    fn frame_cursor_points_at_the_caret() {
        let mut chat = chat_at(80, 24);
        chat.set_input("hello");
        chat.doc.rows = (0..5)
            .map(|i| Row::new(Line::plain(format!("r{i}"))))
            .collect();
        let frame = Frame::assemble(&chat, size(80, 24));
        let (x, y) = frame.cursor.expect("caret visible");
        assert_eq!(x, 7, "❯ + hello");
        let row = row_text(&frame.rows[y as usize]);
        assert_eq!(row, "❯ hello▋");
    }

    /// Flushing: the settled prefix becomes scrollback entries; bubble rows fill the terminal width.
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

    /// Image blocks: the head emits bytes (occupying rows rows), continuations are skipped; without capability, fall back to the placeholder text.
    #[test]
    fn flush_items_emit_one_payload_per_image_block() {
        let mut chat = chat_at(40, 24);
        let img = |url: &str| Line {
            segs: Vec::new(),
            image: Some(ImageRef {
                url: url.into(),
                cols: 4,
                rows: 2,
            }),
        };
        chat.doc.rows = vec![
            Row::new(img("a.png")),
            Row::new(img("a.png")),
            Row::new(Line::plain("text")),
            Row::new(img("b.png")),
        ];
        chat.doc.settled = 4;
        // No capability/cache: the block head falls back to the `#[image]` placeholder; continuations emit nothing.
        let items = flush_items(&chat, 40, chat.doc.settled);
        assert_eq!(items.len(), 3);
        let HistoryItem::Line(head) = &items[0] else {
            panic!("placeholder row");
        };
        assert_eq!(history_text(head), view::IMAGE_PLACEHOLDER);

        // Capable + loaded: one payload per block, row count = image row count.
        chat.image_cap = Some(crate::tui::gfx::ImageCap::default_cells());
        chat.images.insert(
            "a.png".into(),
            std::sync::Arc::new(crate::ui::ImageMeta {
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

    /// Block head/continuation detection (an image block emits bytes exactly once).
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

    /// The core inline invariant: settled content flushes once; afterwards the viewport holds only the tail + chrome.
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
        assert!(
            text.iter().any(|l| l.contains("Welcome back")),
            "首帧含欢迎卡: {text:?}"
        );

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
        assert!(
            text.iter().any(|l| l.contains("? for shortcuts")),
            "chrome 仍在"
        );
    }

    /// The flush cursor counts by message segment: width changes alter every row number without reprinting.
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
        // Another round at the same width: no new settled content → zero items.
        assert!(
            flush_items(&chat, 80, chat.doc.settled).is_empty(),
            "不重复落盘"
        );
        // Narrower rebuild: the segment cursor is unchanged, so still nothing new to flush.
        chat.dirty = true;
        rebuild(&mut chat, size(40, 24), false);
        assert!(
            flush_items(&chat, 40, chat.doc.settled).is_empty(),
            "宽度变化不会让已落盘的段再打印一次"
        );
    }

    /// Inline ctrl+o: full replay — the flush cursor rewinds + the replay flag is set; the replay frame
    /// freezes every settled segment into scrollback, leaving only the dynamic tail and chrome in the viewport.
    #[test]
    fn ctrl_o_replays_the_full_transcript_inline() {
        let mut chat = chat_at(80, 24);
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        // Empty session, everything on screen → no-op: no characters inserted, no replay.
        chat.set_input("hi");
        dispatch_key(
            &mut chat,
            key(KeyCode::Char('o'), KeyModifiers::CONTROL),
            true,
        );
        assert_eq!(chat.input, "hi", "ctrl+o 未插入字符");
        assert!(!chat.dump_transcript, "屏上已是全貌，无需重放");

        // Esc always passes through (menu exits happen inside on_key).
        chat.set_input("/model");
        chat.submit();
        assert!(chat.model_menu.is_some(), "菜单已打开");
        dispatch_key(&mut chat, key(KeyCode::Esc, KeyModifiers::empty()), true);
        assert!(chat.model_menu.is_none(), "Esc 经 gate 退出菜单");

        // A message has flushed → ctrl+o requests the replay; simulate the replay frame: rebuild the full doc
        // and freeze everything up to the last checkpoint.
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
        dispatch_key(
            &mut chat,
            key(KeyCode::Char('o'), KeyModifiers::CONTROL),
            true,
        );
        assert!(chat.dump_transcript, "已落盘内容 → 重放");
        assert!(chat.force_redraw, "重放帧先清可见屏（置顶）");
        assert!(chat.dirty, "重放帧前必然重建");
        chat.dirty = false;
        chat.build_rows(80);
        let mark = chat
            .doc
            .settled_marks
            .last()
            .copied()
            .expect("全量文档有检查点");
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

    /// Release events do not re-trigger (they occur when the terminal reports enhanced keyboards).
    #[test]
    fn key_release_is_ignored() {
        let mut chat = chat_at(80, 24);
        let mut key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty());
        key.kind = KeyEventKind::Release;
        dispatch_key(&mut chat, key, true);
        assert!(chat.input.is_empty());
    }

    /// Full-flow errors must take over the real alternate-screen canvas too, not only
    /// the inline [`Frame::assemble`] seam.
    #[test]
    fn fullscreen_frame_presents_full_error_and_hides_prompt() {
        use crate::error::{ErrorContext, ErrorLevel};
        use crate::tui::chat::ErrorState;

        let mut chat = chat_at(80, 24);
        chat.last_error = Some(ErrorState {
            code: "AUTH_REQUIRED",
            msg: "登录已失效，请重新配置凭据后重试。".to_string(),
            level: ErrorLevel::Full,
            context: ErrorContext::LongTurn,
        });

        let frame = fullscreen_frame(&chat, size(80, 24));
        let text: Vec<String> = frame.rows.iter().map(row_text).collect();
        assert!(text.iter().any(|line| line.contains("出错了")), "{text:?}");
        assert!(
            text.iter().any(|line| line.contains("code=AUTH_REQUIRED")),
            "{text:?}"
        );
        assert!(
            !text
                .iter()
                .any(|line| line.starts_with('╭') || line.starts_with('╰')),
            "全屏错误态不应露出输入框: {text:?}"
        );
        assert!(frame.cursor.is_none(), "全屏错误态隐藏输入光标");
    }

    /// Wheel scrolling and clicks (fullscreen).
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

    /// Lazy flush: nothing freezes when it fits in the window; segments past the window top (including ones crossing it) freeze wholesale.
    #[test]
    fn pick_flush_mark_freezes_only_segments_past_the_window_top() {
        let marks = vec![
            SettledMark {
                row_end: 5,
                segments: 1,
            },
            SettledMark {
                row_end: 20,
                segments: 2,
            },
        ];
        // Everything visible (window starts at 0): nothing freezes.
        assert_eq!(pick_flush_mark(&marks, 0, 0), None);
        // Window top at row 3: segment 1 (0..5) crosses the top → freeze up to 5; segment 2 from 5 on
        // is fully visible → stays live.
        assert_eq!(pick_flush_mark(&marks, 0, 3), Some(marks[0]));
        // Window top at row 10: segment 2 (5..20) also crosses → freeze it too.
        assert_eq!(pick_flush_mark(&marks, 0, 10), Some(marks[1]));
        // After freezing up to 5, the window has not moved further up: do not re-pick a consumed checkpoint.
        assert_eq!(pick_flush_mark(&marks, 5, 5), None);
    }

    /// Settled content stays live inside the window: a small doc freezes nothing, and width changes re-layout on rebuild.
    #[test]
    fn settled_rows_stay_live_while_they_fit() {
        let mut chat = chat_at(80, 24);
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert!(!chat.doc.settled_marks.is_empty(), "欢迎卡有定稿检查点");
        let chrome_len = chrome_rows(&chat, 80, false).rows.len();
        let (win_start, _) = tail_window(chat.doc.rows.len(), chat.tail_start, chrome_len, 24);
        assert_eq!(
            pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start),
            None,
            "装得下就不冻结——欢迎卡留在活文档里可重排"
        );
    }

    /// Transient slash output (e.g. /resume lists) squeezes the window; it must not freeze live content.
    #[test]
    fn transient_slash_output_does_not_freeze_live_rows() {
        let mut chat = chat_at(80, 24);
        chat.slash_lines = (0..40).map(|i| format!("session-{i}")).collect();
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert_eq!(chat.doc.transient_rows, 40);
        let chrome_len = chrome_rows(&chat, 80, false).rows.len();
        let total = chat.doc.rows.len();

        // Regression guard: computing the window over the full doc would misjudge the welcome card as past the top.
        let (naive_start, _) = tail_window(total, chat.tail_start, chrome_len, 24);
        assert!(
            pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, naive_start).is_some(),
            "前提成立：瞬态行确实把窗口挤过了欢迎卡"
        );

        // The production path excludes transient rows: the welcome card stays live.
        let persistent = total - chat.doc.transient_rows;
        let (win_start, _) = tail_window(persistent, chat.tail_start, chrome_len, 24);
        assert_eq!(
            pick_flush_mark(&chat.doc.settled_marks, chat.tail_start, win_start),
            None,
            "瞬态列表只是暂时盖住内容，不是驱逐"
        );
    }

    /// Rehydration: when capacity grows, pull flushed segments back for re-rendering; over budget, roll back.
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

        // Budget is enough: pull the welcome card back (users accept the duplicates when scrolling up).
        chat.rehydrate(80, 24);
        assert_eq!(chat.flushed_segments, 0, "容量够就回灌");
        chat.dirty = true;
        rebuild(&mut chat, size(80, 24), false);
        assert_eq!(chat.doc.rows.len(), welcome_rows, "欢迎卡回到活文档");

        // Not enough budget: rehydration would overflow → roll back, keeping the flushed state.
        chat.advance_flushed();
        chat.rehydrate(80, welcome_rows.saturating_sub(1));
        assert_eq!(chat.flushed_segments, 1, "装不下就不取回");
    }
}
