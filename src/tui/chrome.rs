//! Chrome components: every row below the transcript, declared as [`El`]
//! trees.
//!
//! Each section is a plain function returning an element; [`chrome`] composes
//! them top to bottom. Heights are never predicted — the frame assembler
//! renders the tree and takes the row count ([`crate::tui::el::height`]).
//! The caret is attached to the input line it sits on ([`El::Caret`]) inside
//! [`prompt`], so its frame position falls out of the render walk instead of a
//! hand-counted `prompt_row` offset.

use ratatui::style::Color;

use crate::permission::PermissionMode;
use crate::tui::chat::{
    Chat, ModelMenu, ProviderMenu, ResumeMenu, Row, SlashSuggestion, ThemeMenu, ThinkMenu,
    model_footer_label,
};
use crate::tui::el::El;
use crate::tui::line::{Line, SegStyle, text_width};
use crate::tui::theme::Theme;

/// The currently active selector menu (mutually exclusive: at most one at any time; rendering groups reads of the shell by this).
#[derive(Clone, Copy, Default)]
pub(crate) struct Menus<'a> {
    pub model: Option<&'a ModelMenu>,
    pub think: Option<&'a ThinkMenu>,
    pub theme: Option<&'a ThemeMenu>,
    pub resume: Option<&'a ResumeMenu>,
    pub provider: Option<&'a ProviderMenu>,
}

/// A dim row indented two columns (help / queue / notice / search share it).
pub(crate) fn dim_row(text: impl Into<String>, theme: &Theme) -> Row {
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
    // Menus render before the slash dropdown — the SAME priority as key
    // dispatch. They used to disagree: a menu stayed keyboard-active while
    // the screen showed the dropdown, so Enter landed on an invisible
    // selection.
    {
        let Some(menu) = menus.model else {
            // `/think` level selector (when the model menu is inactive).
            if let Some(think) = menus.think {
                // Thin shell → core: row rendering and the key-hint row delegate to PickerModel (picker-model.md commit A).
                let core = think.picker();
                let mut rows: Vec<Row> = (0..core.items.len())
                    .map(|i| core.row(i, width, theme))
                    .collect();
                rows.push(core.hint_row(crate::tui::chat::ThinkMenu::keys(), width, theme));
                return rows;
            }
            // `/theme` level selector (picker-model.md commit B): the same thin-shell rendering.
            if let Some(theme_menu) = menus.theme {
                let core = theme_menu.picker();
                let mut rows: Vec<Row> = (0..core.items.len())
                    .map(|i| core.row(i, width, theme))
                    .collect();
                rows.push(core.hint_row(crate::tui::chat::ThemeMenu::keys(), width, theme));
                return rows;
            }
            // `/provider` selector (picker-model.md commit D): the same thin-shell rendering.
            if let Some(provider_menu) = menus.provider {
                let core = provider_menu.picker();
                let mut rows: Vec<Row> = (0..core.items.len())
                    .map(|i| core.row(i, width, theme))
                    .collect();
                rows.push(core.hint_row(crate::tui::chat::ProviderMenu::keys(), width, theme));
                return rows;
            }
            // `/resume` session selector (picker-model.md commit C): appends a note row when truncated.
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
                                "(only the latest 20 sessions are shown)",
                                width.saturating_sub(2),
                            )
                        ),
                        SegStyle::fg(theme.inactive),
                    )));
                }
                return rows;
            }
            // No menu open: fall through to the slash dropdown / no-match hint.
            if slash.is_empty() {
                // G9: a bare `/`-query with zero matches gets one dim hint row.
                if no_match {
                    return vec![Row::new(Line::styled(
                        "  (no matching commands · type /help to see the available commands)",
                        SegStyle::fg(theme.inactive),
                    ))];
                }
                return Vec::new();
            }
            return slash_rows(slash, slash_selected, theme, width);
        };
        // `/model` two-level selector: level one `provider` (PickerModel core rendering,
        // with the same endpoint/auth description column as /provider), level two `model` (the same Picker
        // core: ●/❯ dual markers + windowed rendering for long lists; loading / failure / empty each get a note row).
        let Some(m) = &menu.models else {
            let core = menu.provider_picker();
            let mut rows: Vec<Row> = (0..core.items.len())
                .map(|i| core.row(i, width, theme))
                .collect();
            let hint = format!(
                "↑↓/1-{} pick a provider · Enter lists models · Esc exits",
                core.items.len().min(9)
            );
            rows.push(Row::new(Line::styled(
                format!(
                    "  {}",
                    crate::tui::markdown::truncate(&hint, width.saturating_sub(2))
                ),
                SegStyle::fg(theme.inactive),
            )));
            return rows;
        };
        let note = |text: String| {
            vec![Row::new(Line::styled(
                crate::tui::markdown::truncate(&format!("❯ {text}"), width.saturating_sub(2)),
                SegStyle::fg(theme.permission),
            ))]
        };
        if m.loading {
            return note(format!("… fetching {}'s model list", m.provider));
        }
        // Failure reasons are attributed honestly (a 401 used to be swallowed as "the endpoint returned no models").
        if let Some(reason) = &m.failed {
            let mut rows = note(reason.clone());
            rows.push(Row::new(Line::styled(
                "  Esc goes back up",
                SegStyle::fg(theme.inactive),
            )));
            return rows;
        }
        if m.models.is_empty() {
            return note("(the endpoint returned no models; Esc goes back up)".to_string());
        }
        let core = m.picker();
        let mut rows = core.window_rows(crate::tui::chat::SLASH_SUGGESTIONS_MAX + 5, width, theme);
        let hint = format!(
            "↑↓ select · Enter confirms and saves · 1-{} jumps · Esc goes back up",
            core.items.len().min(9)
        );
        rows.push(Row::new(Line::styled(
            format!(
                "  {}",
                crate::tui::markdown::truncate(&hint, width.saturating_sub(2))
            ),
            SegStyle::fg(theme.inactive),
        )));
        rows
    }
}

/// Slash dropdown rows (the no-menu path): windowed around the selection so
/// every command stays reachable from a bare `/` (the old hard cap of 5 hid
/// the rest with no indicator).
fn slash_rows(
    slash: &[SlashSuggestion],
    slash_selected: usize,
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
    let name_col = slash
        .iter()
        .map(|s| s.name.chars().count() + usize::from(!s.hint.is_empty()) + s.hint.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    // Available description width = terminal width - padding(2) - "❯ "(2) - name column - separator(2).
    let desc_width = width.saturating_sub(2 + 2 + name_col + 2).max(8);
    let visible = crate::tui::chat::SLASH_SUGGESTIONS_MAX;
    let (start, end) = if slash.len() <= visible {
        (0, slash.len())
    } else {
        let start = slash_selected
            .saturating_sub(visible / 2)
            .min(slash.len() - visible);
        (start, start + visible)
    };
    let more = |n: usize| {
        Row::new(Line::styled(
            crate::tui::markdown::truncate(&format!("  … {n} more"), width.saturating_sub(2)),
            SegStyle::fg(theme.inactive),
        ))
    };
    let mut rows = Vec::new();
    if start > 0 {
        rows.push(more(start));
    }
    rows.extend(slash[start..end].iter().enumerate().map(|(offset, s)| {
        let i = start + offset;
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
    }));
    if end < slash.len() {
        rows.push(more(slash.len() - end));
    }
    rows
}

/// The suggestion area of a chat (dropdown or the active picker menu).
fn suggestions(chat: &Chat, width: usize) -> El {
    El::Rows(suggestion_rows(
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
        width,
    ))
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

/// Input box (top border + input rows + bottom border) with the caret attached
/// to the input line it sits on.
pub(crate) fn prompt(chat: &Chat, width: usize) -> El {
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
    let mut children = vec![El::Line(Line::styled(
        format!("╭{bar}╮"),
        SegStyle::fg(border_color),
    ))];
    let lines = chat.prompt_lines();
    let (caret_row, caret_col) = caret_cell(chat);
    let caret_row = caret_row.min(lines.len().saturating_sub(1));
    for (i, line) in lines.into_iter().enumerate() {
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
        let el = El::Line(row);
        children.push(if i == caret_row {
            El::caret(caret_col + 2, el)
        } else {
            el
        });
    }
    children.push(El::Line(Line::styled(
        format!("╰{bar}╯"),
        SegStyle::fg(border_color),
    )));
    El::Col(children)
}

/// Every row outside the transcript, top to bottom. `fullscreen` only moves the suggestion area
/// (fullscreen: above the input; inline: below, aligned with slash output).
pub(crate) fn chrome(chat: &Chat, width: usize, fullscreen: bool) -> El {
    let theme = &chat.theme;
    let mut children: Vec<El> = Vec::new();

    if let Some(status) = chat.running_status() {
        children.push(El::Row(status_row(
            &status,
            crate::tui::activities::spinner(chat.tick),
            theme,
        )));
    }
    children.push(El::Lines(chat.task_lines()));
    if let Some(warning) = chat.visible_warning() {
        children.push(El::Line(Line::styled(
            format!("  ⚠ {warning}"),
            SegStyle::fg(theme.warning),
        )));
    }
    for line in chat.help_lines() {
        children.push(El::Row(dim_row(line, theme)));
    }
    // Bottom entity area (agent instances + channels; ctrl+g focuses the selector).
    children.push(El::Lines(chat.entity_rows(width)));

    // Pinned panels (login flows, long-operation progress): persistent until
    // the owning flow unpins them — the one place a device code can wait out
    // its 15 minutes without a TTL eating it.
    for (_, lines) in &chat.pinned_panels {
        for (i, line) in lines.iter().enumerate() {
            let style = if i == 0 {
                SegStyle::fg(theme.claude)
            } else {
                SegStyle::fg(theme.text)
            };
            children.push(El::Line(Line::styled(format!("  {line}"), style)));
        }
    }

    let mut suggestion_area = Some(suggestions(chat, width));
    if fullscreen && let Some(s) = suggestion_area.take() {
        children.push(s);
    }

    children.push(prompt(chat, width));

    if let Some(line) = chat.search_line() {
        children.push(El::Row(dim_row(line, theme)));
    }
    for line in chat.queue_lines() {
        children.push(El::Row(dim_row(line, theme)));
    }
    if let Some(s) = suggestion_area.take() {
        children.push(s);
    }
    if let Some(text) = chat.notice {
        children.push(El::Row(dim_row(text, theme)));
    }
    children.push(El::Row(footer_row(chat, width)));
    if chat.pending_ask.is_some() {
        children.push(El::Row(dim_row("Waiting for permission…", theme)));
    }
    El::Col(children)
}

/// #18 full-flow full-screen error-state skeleton (AC-26/53, ui/ux #68 spec): error title +
/// stable code + description (what happened + what you can do) + primary action (retry/back) + exit hint.
/// Actions are bound at the key layer (chat.rs: Enter=retry, Esc=back on the full-screen state); this function only draws.
pub(crate) fn error_screen(err: &crate::tui::chat::ErrorState, theme: &Theme) -> El {
    El::Lines(vec![
        Line::styled("⚠ something went wrong", SegStyle::fg(theme.error).bold()),
        Line::styled(
            format!("[error] code={}", err.code),
            SegStyle::fg(theme.error),
        ),
        Line::plain(err.msg.clone()),
        Line::plain(""),
        Line::styled(
            "Enter retries · Esc goes back",
            SegStyle::fg(theme.inactive),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::PermissionMode;
    use crate::tui::el;
    use crate::tui::test_util::chat_at;

    fn row_text(row: &Row) -> String {
        row.line.plain_text()
    }

    fn rows_of(el: El) -> Vec<Row> {
        el::render(el).rows
    }

    /// Every chrome section appears in the rendered tree (a table-check test for the old Chrome structure:
    /// now the count is the render itself, so just verify each section really produces rows).
    #[test]
    fn chrome_contains_every_section() {
        let mut chat = chat_at(100, 40);
        let base = rows_of(chrome(&chat, 100, false)).len();
        // Idle: input 3 rows (two borders + one placeholder row) + footer 1 row.
        assert_eq!(base, 4);

        chat.busy = true;
        chat.push_warning("mcp connection failed".to_string());
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
        let rows = rows_of(chrome(&chat, 100, false));
        let text: Vec<String> = rows.iter().map(row_text).collect();
        assert!(
            text.iter().any(|l| l.contains("esc to interrupt")),
            "status row"
        );
        assert!(
            text.iter().any(|l| l.contains("⚠ mcp connection failed")),
            "warning row"
        );
        assert!(text.iter().any(|l| l.contains("shift+tab")), "? panel");
        assert!(
            text.iter().any(|l| l.contains("(reverse-i-search)")),
            "search row"
        );
        assert!(
            text.iter().any(|l| l.contains("> queued message")),
            "queue row"
        );
        assert!(
            text.iter().any(|l| l.contains("Press ctrl-c again")),
            "notice row"
        );
        assert!(
            text.iter().any(|l| l.contains("Waiting for permission…")),
            "ask row"
        );
        assert_eq!(
            text.iter()
                .filter(|l| l.starts_with('╭') || l.starts_with('╰'))
                .count(),
            2,
            "input box top/bottom borders"
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
            provider_descs: vec![String::new(), String::new()],
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
            row_text(&no_match[0]).contains("no matching commands"),
            "{}",
            row_text(&no_match[0])
        );
        // Level one: 2 provider rows + 1 hint row (picker-model.md commit E).
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
            current: None,
            failed: None,
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
            current: None,
            failed: None,
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
        // Level-two failure reason renders honestly (401 is not "no models").
        menu.models = Some(ModelMenuModels {
            provider: "default".into(),
            models: Vec::new(),
            loading: false,
            selected: 0,
            current: None,
            failed: Some("authentication failed: default credentials invalid or not logged in (/provider login default)".into()),
        });
        let failed_rows = suggestion_rows(
            &[],
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
        assert_eq!(failed_rows.len(), 2, "reason row + Esc hint row");
        assert!(
            row_text(&failed_rows[0]).contains("authentication failed"),
            "{}",
            row_text(&failed_rows[0])
        );
        // The level-two model list windows around the selection (visible cap
        // 10) with a "+N" indicator and a hint row — items beyond the window
        // stay reachable by moving the selection.
        menu.models = Some(ModelMenuModels {
            provider: "default".into(),
            models: (0..30).map(|i| format!("m{i}")).collect(),
            loading: false,
            selected: 0,
            current: Some(0),
            failed: None,
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
        assert_eq!(
            think_rows.len(),
            THINK_LEVELS.len() + 1,
            "6 levels + a hint row"
        );
        assert!(
            row_text(&think_rows[0]).contains("● off"),
            "● marks the currently active level: {}",
            row_text(&think_rows[0])
        );
        assert!(
            row_text(&think_rows[1]).starts_with("  ❯"),
            "❯ marks the browsed selection: {}",
            row_text(&think_rows[1])
        );
        assert!(
            row_text(&think_rows[1]).contains("low"),
            "selected row name: {}",
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
            "overlapping rows carry both markers: {}",
            row_text(&rows[3])
        );
        // Hint row (last, dim).
        let hint = row_text(think_rows.last().unwrap());
        assert!(hint.contains("Esc cancels"), "hint row: {hint}");
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
            "the model menu outranks the think menu"
        );
        // Menus take priority over slash suggestions — the same order as key
        // dispatch (they used to disagree: keys went to an invisible menu).
        let slash = vec![SlashSuggestion {
            name: "help".into(),
            hint: String::new(),
            description: "show available commands".into(),
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
        assert!(
            rows.iter().any(|r| row_text(r).contains("m0")),
            "menu visible (rendering and key dispatch in the same order)"
        );
        // Without a menu the dropdown works as usual.
        let rows = suggestion_rows(&slash, 0, Menus::default(), false, &theme, 80);
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
            description: "set the thinking level".into(),
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
        assert!(
            !text.contains("tokens"),
            "0 tokens omit that segment: {text}"
        );

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
        assert!(
            !text.contains("plan mode"),
            "default mode has no badge: {text}"
        );
        // 2 columns of padding on each side (CC footer padding): the model name's right edge lands at width-2.
        assert_eq!(text_width(&text), 78, "model name right-aligned to width-2");

        chat.busy = true;
        let text = row_text(&footer_row(&chat, 80));
        assert!(
            !text.contains("? for shortcuts"),
            "busy keeps only the expand hint: {text}"
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
        assert!(
            !text.contains('▸'),
            "committed state has no preview suffix: {text}"
        );

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
            "preview follows the browsed level: {text}"
        );
        // Esc reverts to the committed badge (no suffix).
        chat.on_key(
            ratatui::crossterm::event::KeyCode::Esc,
            ratatui::crossterm::event::KeyModifiers::empty(),
        );
        let text = row_text(&footer_row(&chat, 80));
        assert!(text.contains("test-model · think high"), "{text}");
        assert!(!text.contains('▸'), "restored after Esc: {text}");
    }

    /// Input box: prefix + `▋` fake caret; the real caret (the tree's Caret
    /// node) lands on the same cell — row offsets fall out of the render walk.
    #[test]
    fn prompt_rows_and_caret_agree() {
        let mut chat = chat_at(80, 24);
        chat.set_input("hi");
        let out = el::render(prompt(&chat, 80));
        assert_eq!(out.rows.len(), 3, "top/bottom borders + one input row");
        assert_eq!(row_text(&out.rows[1]), "❯ hi▋");
        assert_eq!(
            out.caret,
            Some((1, 4)),
            "caret sits on ▋ (the first row after the border)"
        );

        chat.set_input("");
        let out = el::render(prompt(&chat, 80));
        assert_eq!(out.caret, Some((1, 2)));
        assert!(row_text(&out.rows[1]).starts_with("❯ ▋"));

        // Multi-line input: the caret row follows the input row.
        chat.set_input("a\nb\nc");
        let out = el::render(prompt(&chat, 80));
        assert_eq!(out.rows.len(), 5);
        assert_eq!(out.caret, Some((3, 3)));

        chat.bash_mode = true;
        chat.set_input("ls");
        let out = el::render(prompt(&chat, 80));
        assert_eq!(row_text(&out.rows[1]), "! ls▋");
        // bash mode swaps the border color (CC bashBorder).
        assert_eq!(
            out.rows[0].line.segs[0].style.fg,
            Some(chat.theme.bash_border),
            "border color swaps"
        );
    }
}
