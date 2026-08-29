//! The frame, top to bottom: transcript, status, notices, dialog, help,
//! composer, dropdown, footer. Everything below the transcript is measured —
//! each section is built, its rows counted, and whatever is left over goes to
//! the transcript. There is no second height formula to drift out of step.
//!
//! `draw` is pure of everything but the frame it paints.

use bingo_sdk::{LiveTurn, SessionState, View};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::{Frame, style::Style};
use unicode_width::UnicodeWidthStr;

use crate::clock::Now;
use crate::tree::{self, Status, Tree};
use crate::ui::{Picker, Switcher, Ui};
use crate::{dialog, keys, permission, theme, transcript, wrap};

/// How tall the composer box may grow before it scrolls internally.
const COMPOSER_ROWS: usize = 10;
/// How many dropdown rows are shown at once.
const MENU_ROWS: usize = 8;

/// One horizontal band of the frame.
struct Section {
    lines: Vec<Line<'static>>,
    /// A drawn border, for the one section that is a box.
    boxed: bool,
    /// The caret, in cells relative to this section's inner area.
    cursor: Option<(u16, u16)>,
}

impl Section {
    fn lines(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            boxed: false,
            cursor: None,
        }
    }

    fn height(&self) -> u16 {
        let border = if self.boxed { 2 } else { 0 };
        u16::try_from(self.lines.len())
            .unwrap_or(u16::MAX)
            .saturating_add(border)
    }
}

/// One render path for the whole tree: it paints the session in view and
/// derives everything about the others — the band, the `↳` rows, the
/// switcher — from their states.
pub fn draw(tree: &Tree, ui: &Ui, frame: &mut Frame, now: Now) {
    let area = frame.area();
    let width = area.width as usize;
    let sections = fit(chrome(tree, ui, now, width), area.height);
    let used: u16 = sections.iter().map(Section::height).sum();
    let mut y = area.y;
    let rows = area.height.saturating_sub(used);
    render_transcript(tree, ui, frame, Rect::new(area.x, y, area.width, rows), now);
    y += rows;
    for section in sections {
        let height = section.height().min(area.bottom().saturating_sub(y));
        if height == 0 {
            break;
        }
        paint(section, frame, Rect::new(area.x, y, area.width, height));
        y += height;
    }
}

/// Everything below the transcript, in the order it is stacked.
fn chrome(tree: &Tree, ui: &Ui, now: Now, width: usize) -> Vec<Section> {
    let state = tree.viewed();
    let mut out = vec![
        Section::lines(band(tree)),
        Section::lines(status(state, ui, now)),
        Section::lines(notices(ui)),
        Section::lines(ui.block.as_ref().map(block).unwrap_or_default()),
        Section::lines(wrap::wrap_all(&dialog_lines(tree, ui), width)),
        Section::lines(help(ui, width)),
        composer(ui, width),
        Section::lines(menu(ui)),
        Section::lines(vec![footer(state, width)]),
    ];
    out.retain(|s| s.height() > 0);
    out
}

/// Fit the stack into the screen from the bottom up: the composer and the
/// footer are never the ones that go. What does not fit is trimmed from the
/// top of the topmost section that still has room, oldest rows first.
fn fit(sections: Vec<Section>, height: u16) -> Vec<Section> {
    let mut budget = height;
    let mut kept = Vec::new();
    for mut section in sections.into_iter().rev() {
        let want = section.height();
        if want <= budget {
            budget -= want;
            kept.push(section);
            continue;
        }
        if budget > 0 && !section.boxed {
            let drop = section.lines.len() - budget as usize;
            section.lines.drain(..drop);
            kept.push(section);
        }
        break;
    }
    kept.reverse();
    kept
}

fn paint(section: Section, frame: &mut Frame, area: Rect) {
    let inner = if section.boxed {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::dim());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    } else {
        area
    };
    frame.render_widget(Paragraph::new(section.lines), inner);
    if let Some((column, row)) = section.cursor {
        frame.set_cursor_position((
            inner.x + column.min(inner.width.saturating_sub(1)),
            inner.y + row.min(inner.height.saturating_sub(1)),
        ));
    }
}

/// Which session is on screen and who else is in the tree. Nothing while
/// the root is alone, so a session without sub-agents looks as it always did.
fn band(tree: &Tree) -> Vec<Line<'static>> {
    let mut spans = Vec::new();
    if let Some(child) = tree.viewing() {
        spans.push(Span::styled(
            format!("{} {}", theme::CHILD, tree::name(child)),
            theme::accent(),
        ));
        spans.push(Span::styled(
            format!(" · {}", Status::of(child).label()),
            theme::dim(),
        ));
    }
    if let Some(tally) = tree.tally() {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ".to_string(), theme::dim()));
        }
        spans.push(Span::styled(tally, theme::dim()));
    }
    if spans.is_empty() {
        return Vec::new();
    }
    vec![Line::from(spans)]
}

/// The tail of the transcript, or the window the scroll keys parked on.
fn render_transcript(tree: &Tree, ui: &Ui, frame: &mut Frame, area: Rect, now: Now) {
    if area.height == 0 {
        return;
    }
    let all = transcript::lines(
        tree.viewed(),
        &tree.agents(),
        area.width as usize,
        ui.spinner(now.instant),
    );
    let height = area.height as usize;
    let hidden = all.len().saturating_sub(height);
    let start = hidden.saturating_sub(ui.scroll.0);
    let mut shown: Vec<Line<'static>> = all.into_iter().skip(start).take(height).collect();
    // A short transcript hangs from the composer, not from the top of the screen.
    let padding = height - shown.len();
    shown.splice(..0, std::iter::repeat_n(Line::default(), padding));
    frame.render_widget(Paragraph::new(shown), area);
}

/// Only while a turn runs: what it is doing, how long, and how to stop it.
fn status(state: &SessionState, ui: &Ui, now: Now) -> Vec<Line<'static>> {
    let Some(turn) = state.turn.as_ref() else {
        return Vec::new();
    };
    let elapsed = now.wall.duration_since(turn.started_at).as_secs().max(0);
    let mut spans = vec![
        Span::styled(format!("{} ", ui.spinner(now.instant)), theme::accent()),
        Span::raw(activity(state, turn)),
        Span::styled(format!(" (esc to interrupt · {elapsed}s)"), theme::dim()),
    ];
    if let Some(retry) = turn.retrying {
        spans.push(Span::styled(
            format!(" retrying {}/{}", retry.attempt, retry.max),
            theme::caution(),
        ));
    }
    let mut out = vec![Line::from(spans)];
    out.extend(
        state
            .queue
            .iter()
            .map(|entry| Line::from(Span::styled(format!("> {}", entry.preview), theme::dim()))),
    );
    out
}

/// The running tool's name, else the plain verb.
fn activity(state: &SessionState, turn: &LiveTurn) -> String {
    state
        .items
        .iter()
        .rev()
        .find(|item| {
            item.turn.as_ref() == Some(&turn.id) && item.status == bingo_sdk::ItemStatus::Running
        })
        .and_then(|item| match &item.body {
            bingo_sdk::ItemBody::ToolCall { name, .. } => Some(name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "Working…".to_string())
}

/// The `?` panel: the one binding table, then the commands this session can
/// run — the surface's own and the kernel's, from the same list the dropdown
/// ranks.
fn help(ui: &Ui, width: usize) -> Vec<Line<'static>> {
    if !ui.help {
        return Vec::new();
    }
    let commands = ui.commands();
    let column = commands.iter().map(|c| c.name.width()).max().unwrap_or(0);
    let mut out = keys::help_lines(width);
    out.push(Line::default());
    out.extend(commands.iter().map(|spec| {
        Line::from(Span::styled(
            format!("/{:<column$}  {}", spec.name, spec.hint, column = column),
            theme::dim(),
        ))
    }));
    out
}

fn notices(ui: &Ui) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = ui
        .notices
        .iter()
        .map(|notice| {
            Line::from(Span::styled(
                notice.text.clone(),
                theme::level(notice.level),
            ))
        })
        .collect();
    if ui.opening {
        out.push(Line::from(Span::styled(
            "opening a session…".to_string(),
            theme::dim(),
        )));
    }
    out
}

/// A command's `View`, shown until the next key.
fn block(view: &View) -> Vec<Line<'static>> {
    match view {
        View::Text { text } => text.lines().map(plain).collect(),
        View::List { items } => items.iter().map(|i| plain(&format!("• {i}"))).collect(),
        View::Table { headers, rows } => table(headers, rows),
    }
}

fn table(headers: &[String], rows: &[Vec<String>]) -> Vec<Line<'static>> {
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(i, header)| {
            rows.iter()
                .filter_map(|row| row.get(i))
                .map(|cell| cell.width())
                .chain(std::iter::once(header.width()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = vec![Line::from(Span::styled(
        row(headers, &widths),
        theme::bold(),
    ))];
    out.extend(rows.iter().map(|cells| plain(&row(cells, &widths))));
    out
}

fn row(cells: &[String], widths: &[usize]) -> String {
    cells
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            format!(
                "{cell:<width$}",
                width = widths.get(i).copied().unwrap_or(0)
            )
        })
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_string()
}

/// The dialog slot: a picker, the switcher, or the open interaction — the
/// tree's first, which may be a child's.
fn dialog_lines(tree: &Tree, ui: &Ui) -> Vec<Line<'static>> {
    if let Some(picker) = ui.picker.as_ref() {
        return picker_lines(picker);
    }
    if let Some(switcher) = ui.switcher.as_ref() {
        return switcher_lines(tree, switcher);
    }
    let Some((owner, interaction)) = tree.open_interaction() else {
        return Vec::new();
    };
    let asked_elsewhere = owner.summary.id != *tree.view();
    let agent = asked_elsewhere.then(|| tree::name(owner));
    dialog::lines(&ui.dialog, interaction, agent.as_deref())
}

/// The `ctrl+g` list: the root and its agents, with what each is doing.
fn switcher_lines(tree: &Tree, switcher: &Switcher) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled(
        "Sessions".to_string(),
        theme::accent().patch(theme::bold()),
    ))];
    for (index, row) in tree.rows().iter().enumerate() {
        let selected = index == switcher.selected;
        let style = if selected {
            theme::accent()
        } else {
            theme::dim()
        };
        let mark = if row.attention { theme::THINKING } else { "" };
        out.push(Line::from(Span::styled(
            format!(
                "{} {mark}{} · {}",
                if selected { "❯" } else { " " },
                row.name,
                row.status.label()
            ),
            style,
        )));
    }
    out.push(Line::from(Span::styled(
        "  enter to switch · esc to close".to_string(),
        theme::dim(),
    )));
    out
}

fn picker_lines(picker: &Picker) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled(
        "Resume".to_string(),
        theme::accent().patch(theme::bold()),
    ))];
    for (index, session) in picker.sessions.iter().enumerate() {
        let style = if index == picker.selected {
            theme::accent()
        } else {
            theme::dim()
        };
        let title = session.title.clone().unwrap_or_else(|| "untitled".into());
        out.push(Line::from(Span::styled(
            format!(
                "{} {}. {title} · {} · {}",
                if index == picker.selected { "❯" } else { " " },
                index + 1,
                session.updated_at,
                session.id
            ),
            style,
        )));
    }
    out.push(Line::from(Span::styled(
        "  enter to open · esc to cancel".to_string(),
        theme::dim(),
    )));
    out
}

/// The prompt box: the caret lives here and nowhere else.
fn composer(ui: &Ui, width: usize) -> Section {
    // Two border columns, then the `❯ ` prompt.
    let inner = width.saturating_sub(2 + theme::USER.width()).max(1);
    let layout = ui.composer.layout(inner);
    // Scroll only as far as the caret needs: it must stay in the box.
    let start = layout.cursor.0.saturating_sub(COMPOSER_ROWS - 1);
    let lines: Vec<Line<'static>> = layout
        .lines
        .iter()
        .enumerate()
        .skip(start)
        .take(COMPOSER_ROWS)
        .map(|(i, text)| prompt_line(i, text, ui))
        .collect();
    Section {
        lines,
        boxed: true,
        cursor: Some((
            u16::try_from(layout.cursor.1 + theme::USER.width()).unwrap_or(u16::MAX),
            u16::try_from(layout.cursor.0 - start).unwrap_or(u16::MAX),
        )),
    }
}

fn prompt_line(index: usize, text: &str, ui: &Ui) -> Line<'static> {
    let lead = if index == 0 { theme::USER } else { "  " };
    if index == 0 && ui.composer.is_empty() {
        return Line::from(vec![
            Span::styled(lead, theme::accent()),
            Span::styled(keys::PLACEHOLDER, theme::dim()),
        ]);
    }
    Line::from(vec![
        Span::styled(lead, theme::accent()),
        Span::raw(text.to_string()),
    ])
}

fn menu(ui: &Ui) -> Vec<Line<'static>> {
    let rows = ui.suggestions();
    let selected = ui.menu.selected.min(rows.len().saturating_sub(1));
    let column = rows.iter().map(|r| r.label.width()).max().unwrap_or(0);
    rows.iter()
        .enumerate()
        .take(MENU_ROWS)
        .map(|(index, row)| {
            let style = if index == selected {
                theme::selected()
            } else {
                theme::dim()
            };
            let label = format!("{:<column$}", row.label, column = column);
            Line::from(Span::styled(
                format!("  {label}  {}", row.hint).trim_end().to_string(),
                style,
            ))
        })
        .collect()
}

/// The mode and the hints on the left, what the next turn will cost on the
/// right.
fn footer(state: &SessionState, width: usize) -> Line<'static> {
    let left = hints(state);
    let right = badges(state);
    let taken: usize = left
        .iter()
        .chain(right.iter())
        .map(|s| s.content.width())
        .sum();
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(width.saturating_sub(taken).max(1))));
    spans.extend(right);
    Line::from(spans)
}

/// The permission mode the policy published, then the chords.
fn hints(state: &SessionState) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    if let Some((mode, style)) = permission_badge(state) {
        out.push(Span::styled(mode, style));
        out.push(Span::styled(" · ", theme::dim()));
    }
    out.push(Span::styled(
        format!("{} · {}", keys::FOOTER_HINT, keys::FOOTER_MODES),
        theme::dim(),
    ));
    out
}

/// The model and how full its context is.
fn badges(state: &SessionState) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let (context, style) = context_badge(state);
    if let Some(model) = state.summary.model.clone().filter(|m| !m.is_empty()) {
        let gap = if context.is_empty() { "" } else { " " };
        out.push(Span::styled(format!("{model}{gap}"), theme::dim()));
    }
    if !context.is_empty() {
        out.push(Span::styled(context, style));
    }
    out
}

/// The mode chip, absent until a policy publishes one. `bypassPermissions` is
/// the one mode that turns the gate off, so it is the one that catches the eye;
/// `default` is what a session already is, so it says so quietly.
fn permission_badge(state: &SessionState) -> Option<(String, Style)> {
    let mode = permission::mode(state)?;
    let style = match mode {
        "bypassPermissions" => theme::caution(),
        "default" => theme::dim(),
        _ => theme::accent(),
    };
    Some((mode.to_string(), style))
}

fn context_badge(state: &SessionState) -> (String, Style) {
    let Some(context) = state.context else {
        return (String::new(), theme::dim());
    };
    let style = if context.used >= context.trigger && context.trigger > 0 {
        theme::danger()
    } else {
        theme::dim()
    };
    (format!("ctx {}%", context.percent()), style)
}

fn plain(text: &str) -> Line<'static> {
    Line::from(Span::raw(text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use crate::ui::Picker;
    use bingo_sdk::{
        ContentPart, ContextUsage, Event, InterruptReason, ItemBody, ItemStatus, KernelError,
        Level, Preview, QueueEntry, SessionSummary, ToolOutput, TurnId, TurnStatus,
    };
    use serde_json::json;

    fn item_frame(seq: u64, item: bingo_sdk::Item) -> bingo_sdk::Frame {
        frame(seq, Event::ItemCompleted { item })
    }

    #[test]
    fn idle() {
        let state = folded(vec![
            item_frame(1, user("itm_1", "run the tests")),
            item_frame(
                2,
                assistant(
                    "itm_2",
                    "All 33 pass.\n\n- `wrap` is done\n- `keys` is done",
                    ItemStatus::Completed,
                ),
            ),
        ]);
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn streaming_assistant_text() {
        let state = folded(vec![
            frame(1, started("trn_1")),
            item_frame(2, user("itm_1", "explain")),
            frame(
                3,
                Event::ItemStarted {
                    item: assistant("itm_2", "", ItemStatus::Running),
                },
            ),
            frame(
                4,
                Event::ItemDelta {
                    item: bingo_sdk::ItemId::from_raw("itm_2"),
                    n: 0,
                    kind: bingo_sdk::DeltaKind::Text,
                    data: "# Heading\n\nA half-written **sen".into(),
                },
            ),
        ]);
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn a_running_tool_shows_its_tail() {
        let state = folded(vec![
            frame(1, started("trn_1")),
            frame(
                2,
                Event::ItemStarted {
                    item: running_tool("itm_1", "Bash", "compiling bingo-surface-tui…"),
                },
            ),
        ]);
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn a_completed_tool_shows_the_first_lines_of_its_output() {
        let output = ToolOutput {
            parts: vec![ContentPart::text(
                (1..=9).map(|i| format!("line {i}\n")).collect::<String>(),
            )],
            is_error: false,
            display: None,
        };
        let state = folded(vec![item_frame(
            1,
            tool(
                "itm_1",
                "Read",
                json!({"file_path": "src/lib.rs"}),
                Some(output),
                ItemStatus::Completed,
            ),
        )]);
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn a_failed_tool_and_a_failed_turn_are_both_red() {
        let state = folded(vec![
            item_frame(
                1,
                tool(
                    "itm_1",
                    "Bash",
                    json!({"command": "cargo test"}),
                    Some(ToolOutput::error("exit 101")),
                    ItemStatus::Failed,
                ),
            ),
            frame(
                2,
                completed(
                    "trn_1",
                    TurnStatus::Failed {
                        error: KernelError::new(
                            bingo_sdk::ErrorCode::ProviderUnavailable,
                            "no route to the provider",
                        ),
                    },
                ),
            ),
        ]);
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn a_diff_result_is_coloured_by_column() {
        let state = folded(vec![item_frame(
            1,
            tool(
                "itm_1",
                "Edit",
                json!({"file_path": "src/lib.rs"}),
                Some(diff_output()),
                ItemStatus::Completed,
            ),
        )]);
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn permission_collapsed() {
        let state = folded(vec![frame(
            1,
            opened(permission(Some("Edit(src/)"), Some(long_diff()))),
        )]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn permission_expanded() {
        let state = folded(vec![frame(
            1,
            opened(permission(Some("Edit(src/)"), Some(long_diff()))),
        )]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        crate::input::on_key(&mut ui, &solo(&state), ctrl('e'), now);
        insta::assert_snapshot!(draw_sized(80, 34, &state, &ui, now));
    }

    #[test]
    fn permission_with_the_feedback_row_open() {
        let state = folded(vec![frame(
            1,
            opened(permission(
                None,
                Some(Preview::Command {
                    command: "rm -rf build".into(),
                    cwd: "/tmp/project".into(),
                }),
            )),
        )]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        crate::input::on_key(&mut ui, &solo(&state), typed('n'), now);
        write(&mut ui, &state, "use cargo clean", now);
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn question_single() {
        let state = folded(vec![frame(1, opened(question(false, false)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn question_multi() {
        let state = folded(vec![frame(1, opened(question(true, true)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        crate::input::on_key(&mut ui, &solo(&state), typed(' '), now);
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn confirm_dialog() {
        let state = folded(vec![frame(1, opened(confirm()))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn help_panel() {
        let state = state();
        let (mut ui, now) = scene();
        ui.help = true;
        insta::assert_snapshot!(draw_sized(100, 28, &state, &ui, now));
    }

    #[test]
    fn dropdown() {
        let state = state();
        let (mut ui, now) = scene();
        ui.catalog = vec![bingo_sdk::CommandSpec {
            name: "model".into(),
            aliases: vec![],
            hint: "[provider/]model".into(),
            args: bingo_sdk::ArgSpec::Catalog {
                source: "models".into(),
            },
            instant: true,
            family: "kernel".into(),
        }];
        write(&mut ui, &state, "/", now);
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn the_first_ctrl_c_says_how_to_leave() {
        let state = state();
        let (mut ui, now) = scene();
        crate::input::on_key(&mut ui, &solo(&state), ctrl('c'), now);
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn a_retrying_turn_says_which_attempt() {
        let state = folded(vec![
            frame(1, started("trn_1")),
            frame(
                2,
                Event::TurnRetrying {
                    turn: TurnId::from_raw("trn_1"),
                    attempt: 2,
                    max: 10,
                    delay_ms: 500,
                    dropped: vec![],
                    reason: "server error 503".into(),
                },
            ),
            frame(
                3,
                Event::QueueChanged {
                    revision: 1,
                    entries: vec![QueueEntry {
                        intent: bingo_sdk::IntentId::from_raw("req_2"),
                        position: 0,
                        preview: "also fix the docs".into(),
                        steerable: true,
                        origin: bingo_sdk::Origin::surface("tui"),
                    }],
                },
            ),
        ]);
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn an_interrupted_turn_keeps_its_marker() {
        let state = folded(vec![
            item_frame(1, user("itm_1", "long job")),
            item_frame(
                2,
                item(
                    "itm_2",
                    ItemStatus::Completed,
                    ItemBody::Interruption {
                        marker: "[interrupted by the user]".into(),
                    },
                ),
            ),
            frame(
                3,
                completed(
                    "trn_1",
                    TurnStatus::Interrupted {
                        reason: InterruptReason::UserCancel,
                    },
                ),
            ),
        ]);
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    fn with_context(used: u64) -> bingo_sdk::SessionState {
        folded(vec![frame(
            1,
            Event::TurnUsage {
                turn: TurnId::from_raw("trn_1"),
                usage: Default::default(),
                context: ContextUsage {
                    used,
                    window: 200_000,
                    trigger: 180_000,
                },
            },
        )])
    }

    #[test]
    fn the_context_badge_is_plain_below_the_trigger() {
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&with_context(120_000), &ui, now));
    }

    #[test]
    fn the_context_badge_is_red_at_the_trigger() {
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&with_context(185_000), &ui, now));
    }

    #[test]
    fn the_footer_names_the_mode_the_policy_published() {
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&with_permission_mode("acceptEdits"), &ui, now));
    }

    #[test]
    fn the_footer_cautions_about_bypassing_the_gate() {
        let state = with_permission_mode("bypassPermissions");
        let (ui, now) = scene();
        assert_eq!(
            permission_badge(&state).map(|(_, style)| style),
            Some(theme::caution()),
            "the one mode that turns the gate off is the one that is coloured"
        );
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn a_config_without_a_mode_leaves_the_footer_as_it_was() {
        let published = folded(vec![frame(1, plugin_view("hooks", json!({"events": 3})))]);
        let (ui, now) = scene();
        assert_eq!(
            render(&published, &ui, now),
            render(&state(), &ui, now),
            "no badge until a policy publishes one"
        );
    }

    #[test]
    fn a_rejected_intent_becomes_a_notice() {
        let state = state();
        let (mut ui, now) = scene();
        ui.notify(Level::Error, "unknown command: /x", now.instant);
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn a_view_table_sits_above_the_composer() {
        let state = state();
        let (mut ui, now) = scene();
        ui.block = Some(View::Table {
            headers: vec!["mode".into(), "meaning".into()],
            rows: vec![
                vec!["default".into(), "ask for what is not allowed".into()],
                vec![
                    "acceptEdits".into(),
                    "edits inside the cwd are allowed".into(),
                ],
            ],
        });
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn the_session_picker_lists_what_can_be_resumed() {
        let state = state();
        let (mut ui, now) = scene();
        ui.picker = Some(Picker {
            sessions: vec![
                SessionSummary {
                    title: Some("fix the parser".into()),
                    ..summary()
                },
                SessionSummary {
                    id: bingo_sdk::SessionId::from_raw("ses_2"),
                    title: None,
                    ..summary()
                },
            ],
            selected: 0,
        });
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    // ---- the tree -------------------------------------------------------

    /// A transcript whose tool call spawned `reviewer`, and the child's own
    /// frames after it, in the order one stream delivers them.
    fn spawned(child: Vec<bingo_sdk::Frame>) -> crate::tree::Tree {
        let mut frames = vec![
            item_frame(1, user("itm_0", "have it reviewed")),
            item_frame(
                2,
                tool(
                    "itm_1",
                    "SpawnAgent",
                    json!({"prompt": "review the diff"}),
                    Some(ToolOutput {
                        parts: vec![ContentPart::text("reviewer started")],
                        is_error: false,
                        display: None,
                    }),
                    ItemStatus::Completed,
                ),
            ),
            child_frame(1, announced("reviewer")),
        ];
        frames.extend(child);
        folded_tree(frames)
    }

    #[test]
    fn a_tool_call_that_spawned_an_agent_says_what_it_is_doing() {
        let tree = spawned(vec![child_frame(2, started("trn_9"))]);
        let (ui, now) = scene();
        let screen = render_tree(&tree, &ui, now);
        assert!(screen.contains("↳ reviewer · running"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn a_child_that_needs_a_person_is_counted_in_the_band() {
        let tree = spawned(vec![child_frame(2, opened(child_permission()))]);
        let (mut ui, now) = scene();
        ui.dialog
            .focus_on(tree.open_interaction().map(|(_, open)| open));
        let screen = render_tree(&tree, &ui, now);
        assert!(screen.contains("1 agent · 1 needs you"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn the_view_of_a_child_is_its_own_transcript_under_its_own_name() {
        let mut tree = spawned(vec![
            child_frame(2, started("trn_9")),
            child_frame(
                3,
                Event::ItemCompleted {
                    item: user("itm_9", "review the diff"),
                },
            ),
            child_frame(
                4,
                Event::ItemCompleted {
                    item: assistant("itm_10", "Two nits, otherwise fine.", ItemStatus::Completed),
                },
            ),
        ]);
        tree.show(&child_id());
        let (ui, now) = scene();
        let screen = render_tree(&tree, &ui, now);
        assert!(
            screen.contains("↳ reviewer · running · 1 agent"),
            "{screen}"
        );
        assert!(screen.contains("Two nits"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn the_switcher_lists_the_root_and_its_agents() {
        let tree = spawned(vec![child_frame(2, opened(child_permission()))]);
        let (mut ui, now) = scene();
        ui.switcher = Some(Switcher { selected: 1 });
        insta::assert_snapshot!(render_tree(&tree, &ui, now));
    }

    #[test]
    fn the_composer_survives_a_screen_too_small_for_the_chrome() {
        let state = folded(vec![frame(
            1,
            opened(permission(Some("E(s/)"), Some(long_diff()))),
        )]);
        let (mut ui, now) = scene();
        ui.help = true;
        ui.dialog.focus_on(state.interactions.first());
        let screen = draw_sized(60, 12, &state, &ui, now);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[rows.len() - 4].contains('\u{256d}'), "{screen}");
        assert!(rows[rows.len() - 3].contains('\u{276f}'), "{screen}");
        assert!(rows[rows.len() - 2].contains('\u{256f}'), "{screen}");
        assert!(rows[rows.len() - 1].contains("? for shortcuts"), "{screen}");
    }

    #[test]
    fn a_terminal_too_small_for_anything_still_draws() {
        let (ui, now) = scene();
        for (width, height) in [(1u16, 1u16), (4, 2), (10, 3), (20, 5)] {
            draw_sized(width, height, &state(), &ui, now);
        }
    }

    #[test]
    fn the_transcript_scrolls_back_a_page() {
        let state = folded(
            (1..=30)
                .map(|i| item_frame(i, user(&format!("itm_{i}"), &format!("line {i}"))))
                .collect(),
        );
        let (mut ui, now) = scene();
        let bottom = render(&state, &ui, now);
        crate::input::on_key(
            &mut ui,
            &solo(&state),
            key(crossterm::event::KeyCode::PageUp),
            now,
        );
        let scrolled = render(&state, &ui, now);
        assert_ne!(bottom, scrolled, "page up must move the window");
        insta::assert_snapshot!(scrolled);
    }
}
