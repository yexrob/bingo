//! The frame: the regions of [`crate::frame`] filled in, and the layers over
//! them. Nothing sits above the transcript, and nothing below it moves — the
//! input box and the status line are cut from the bottom before the transcript
//! is given what is left, so a dialog opening or a notice arriving never
//! shifts a row a person was reading.
//!
//! `draw` is pure of everything but the frame it paints.

use bingo_sdk::{Driver, LiveTurn, SessionState};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::clock::Now;
use crate::frame::{self, Demand, Regions};
use crate::tree::{self, Tree};
use crate::ui::{Picker, Switcher, Ui};
use crate::{block, dialog, keys, panel, status, theme, wrap};

/// How tall the composer box may grow before it scrolls internally.
const COMPOSER_ROWS: usize = 10;
/// How many dropdown rows are shown at once.
const MENU_ROWS: usize = 8;

/// One render path for the whole tree: it paints the session in view and
/// derives everything about the others — the counts on the status line, the
/// `↳` rows, the switcher — from their states.
pub fn draw(tree: &Tree, ui: &Ui, frame: &mut Frame, now: Now) {
    let area = frame.area();
    let regions = frame::regions(area, demand(tree, ui, area.width, now));
    ui.painted.borrow_mut().regions = regions;
    render_transcript(tree, ui, frame, regions.transcript, now);
    render_activity(tree.viewed(), ui, frame, regions.activity, now);
    render_composer(tree.viewed(), ui, frame, regions.composer);
    render_status(tree, ui, frame, regions.status);
    layers(tree, ui, frame, regions);
}

/// What the frame must make room for before the transcript is given the rest.
fn demand(tree: &Tree, ui: &Ui, width: u16, now: Now) -> Demand {
    let state = tree.viewed();
    Demand {
        composer: u16::try_from(composer_rows(ui, width as usize)).unwrap_or(u16::MAX),
        activity: u16::try_from(activity(state, ui, now).len()).unwrap_or(u16::MAX),
        rail: false,
    }
}

/// The rows the draft needs, at most [`COMPOSER_ROWS`].
fn composer_rows(ui: &Ui, width: usize) -> usize {
    ui.composer
        .layout(inner_width(width))
        .lines
        .len()
        .clamp(1, COMPOSER_ROWS)
}

/// The cells inside the box, once its border and the `❯ ` are taken.
fn inner_width(width: usize) -> usize {
    width.saturating_sub(2 + theme::USER.width()).max(1)
}

fn render_status(tree: &Tree, ui: &Ui, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let line = status::line(tree, ui, area.width as usize);
    frame.render_widget(Paragraph::new(vec![line]), area);
}

/// The activity row and whatever is queued behind it.
fn render_activity(state: &SessionState, ui: &Ui, frame: &mut Frame, area: Rect, now: Now) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(Paragraph::new(activity(state, ui, now)), area);
}

/// The transient things that float over the frame. They are layers, not rows:
/// the input box never moves to make room for one.
fn layers(tree: &Tree, ui: &Ui, frame: &mut Frame, regions: Regions) {
    let width = regions.transcript.width as usize;
    let lines: Vec<Line<'static>> = [
        ui.block.as_ref().map(block::lines).unwrap_or_default(),
        wrap::wrap_all(&dialog_lines(tree, ui), width),
        help(ui, width),
        plugin_state(tree.viewed(), ui),
        menu(ui),
    ]
    .concat();
    if lines.is_empty() {
        return;
    }
    let over = regions.above();
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .min(over.height);
    if height == 0 {
        return;
    }
    // What does not fit is trimmed from the top: the newest rows are the ones
    // that were opened.
    let dropped = lines.len() - height as usize;
    let area = Rect {
        y: over.bottom() - height,
        height,
        ..over
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines[dropped..].to_vec()), area);
}

/// The tail of the transcript, or the window the scroll keys parked on. What
/// it drew is left in [`crate::ui::Painted`] for the next key to read.
fn render_transcript(tree: &Tree, ui: &Ui, frame: &mut Frame, area: Rect, now: Now) {
    if area.height == 0 {
        return;
    }
    let rows = area.height as usize;
    let mut painted = ui.painted.borrow_mut();
    painted.height = painted.blocks.sync(
        tree.viewed(),
        &tree.agents(),
        area.width as usize,
        ui.spinner(now.instant),
    );
    painted.top = ui.scroll.top(painted.height, rows, now.instant);
    let mut shown = painted.blocks.window(painted.top, rows);
    // A short transcript hangs from the composer, not from the top of the screen.
    let padding = rows - shown.len();
    shown.splice(..0, std::iter::repeat_n(Line::default(), padding));
    frame.render_widget(Paragraph::new(shown), area);
}

/// Only while a turn runs: what it is doing, how long, and how to stop it.
fn activity(state: &SessionState, ui: &Ui, now: Now) -> Vec<Line<'static>> {
    let Some(turn) = state.turn.as_ref() else {
        return Vec::new();
    };
    let elapsed = now.wall.duration_since(turn.started_at).as_secs().max(0);
    let mut spans = vec![
        Span::styled(format!("{} ", ui.spinner(now.instant)), theme::accent()),
        Span::raw(verb(state, turn)),
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
fn verb(state: &SessionState, turn: &LiveTurn) -> String {
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

/// The `ctrl+t` panel: whatever the plugins wrote into the session in view.
fn plugin_state(state: &SessionState, ui: &Ui) -> Vec<Line<'static>> {
    if !ui.panel {
        return Vec::new();
    }
    panel::lines(state)
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
                "{} {mark}{}{}",
                if selected { "❯" } else { " " },
                row.name,
                tree::suffix(row.status)
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

/// The prompt box: the caret lives here and nowhere else. Its border is the
/// one box the frame draws itself.
fn render_composer(state: &SessionState, ui: &Ui, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::dim());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let layout = ui.composer.layout(inner_width(area.width as usize));
    // Scroll only as far as the caret needs: it must stay in the box.
    let start = layout.cursor.0.saturating_sub(COMPOSER_ROWS - 1);
    let placeholder = placeholder(state);
    let lines: Vec<Line<'static>> = layout
        .lines
        .iter()
        .enumerate()
        .skip(start)
        .take(COMPOSER_ROWS)
        .map(|(i, text)| prompt_line(i, text, ui, &placeholder))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    frame.set_cursor_position((
        inner.x
            + u16::try_from(layout.cursor.1 + theme::USER.width())
                .unwrap_or(u16::MAX)
                .min(inner.width.saturating_sub(1)),
        inner.y
            + u16::try_from(layout.cursor.0 - start)
                .unwrap_or(u16::MAX)
                .min(inner.height.saturating_sub(1)),
    ));
}

/// What the empty composer offers. Nothing answers a `Log` session, so it is
/// posted into rather than asked (ADR-0011 §1).
fn placeholder(state: &SessionState) -> String {
    match state.summary.driver {
        Driver::Log => format!("post to {}", tree::name(state)),
        Driver::Model => keys::PLACEHOLDER.to_string(),
    }
}

fn prompt_line(index: usize, text: &str, ui: &Ui, placeholder: &str) -> Line<'static> {
    let lead = if index == 0 { theme::USER } else { "  " };
    if index == 0 && ui.composer.is_empty() {
        return Line::from(vec![
            Span::styled(lead, theme::accent()),
            Span::styled(placeholder.to_string(), theme::dim()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use crate::ui::Picker;
    use bingo_sdk::{
        ContentPart, ContextUsage, Event, InterruptReason, ItemBody, ItemStatus, KernelError,
        Level, Preview, QueueEntry, SessionSummary, ToolOutput, TurnId, TurnStatus, View,
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
    fn login_browser_dialog() {
        let state = folded(vec![frame(
            1,
            opened(login(bingo_sdk::LoginFlow::Browser {
                url: "https://auth.openai.com/oauth/authorize?client_id=app_x&state=s1".into(),
            })),
        )]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn login_device_dialog() {
        let state = folded(vec![frame(
            1,
            opened(login(bingo_sdk::LoginFlow::Device {
                url: "https://auth.openai.com/codex/device".into(),
                code: "ABCD-EFGH".into(),
            })),
        )]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn login_paste_dialog_with_the_words_row_open() {
        let state = folded(vec![frame(1, opened(login(bingo_sdk::LoginFlow::Paste)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        crate::input::on_key(&mut ui, &solo(&state), typed('1'), now);
        write(&mut ui, &state, "sk-pasted-elsewhere", now);
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
        ui.catalogs.commands = vec![bingo_sdk::CommandSpec {
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
    fn the_context_notice_sits_on_the_status_line_when_it_is_true() {
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&with_context(170_000), &ui, now));
    }

    #[test]
    fn the_status_line_names_the_mode_the_policy_published() {
        let (ui, now) = scene();
        insta::assert_snapshot!(render(&with_permission_mode("acceptEdits"), &ui, now));
    }

    #[test]
    fn a_config_without_a_mode_leaves_the_status_line_as_it_was() {
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
    fn a_child_that_needs_a_person_is_counted_on_the_status_line() {
        let tree = spawned(vec![child_frame(2, opened(child_permission()))]);
        let (mut ui, now) = scene();
        ui.dialog
            .focus_on(tree.open_interaction().map(|(_, open)| open));
        let screen = render_tree(&tree, &ui, now);
        assert!(screen.contains("1 needs you (ctrl+g)"), "{screen}");
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
        assert!(screen.contains("in reviewer · fake-1"), "{screen}");
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

    // ---- a session nothing answers --------------------------------------

    /// A room under the root, with the room in view: what a member of it sees.
    fn room(frames: Vec<bingo_sdk::Frame>) -> crate::tree::Tree {
        let mut all = vec![log_frame(1, log_announced("#design"))];
        all.extend(frames);
        let mut tree = folded_tree(all);
        tree.show(&log_id());
        tree
    }

    fn posted(seq: u64, id: &str, principal: &str, text: &str) -> bingo_sdk::Frame {
        log_frame(
            seq,
            Event::ItemCompleted {
                item: post(id, principal, text),
            },
        )
    }

    #[test]
    fn a_room_transcript_reads_as_a_chat() {
        let tree = room(vec![
            posted(2, "itm_1", "reviewer", "the plan is thin on tests"),
            posted(3, "itm_2", "scout", "M9's exit criteria cover them"),
            log_frame(
                4,
                Event::ItemCompleted {
                    item: user("itm_3", "then let us ship it"),
                },
            ),
        ]);
        let (ui, now) = scene();
        let screen = render_tree(&tree, &ui, now);
        assert!(screen.contains("reviewer: the plan"), "{screen}");
        assert!(screen.contains("scout: M9's"), "{screen}");
        assert!(
            !screen.contains("running") && !screen.contains("idle"),
            "nothing answers a room: {screen}"
        );
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn the_composer_of_a_room_offers_to_post_to_it() {
        let (ui, now) = scene();
        let screen = render_tree(&room(vec![]), &ui, now);
        assert!(screen.contains("post to #design"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn a_room_sits_in_the_switcher_with_no_status() {
        let mut tree = room(vec![child_frame(1, announced("reviewer"))]);
        let root = tree.root_id().clone();
        tree.show(&root);
        let (mut ui, now) = scene();
        ui.switcher = Some(Switcher { selected: 1 });
        insta::assert_snapshot!(render_tree(&tree, &ui, now));
    }

    // ---- the plugin-state panel -----------------------------------------

    fn tasks() -> Event {
        extended(
            "bingo.tasks",
            "tasks",
            json!([
                {"id": 1, "status": "pending", "subject": "write the plan"},
                {"id": 2, "status": "in_progress", "subject": "ship it", "owner": "reviewer"},
            ]),
        )
    }

    fn members() -> Event {
        extended(
            "bingo.rooms",
            "members",
            json!({"members": ["reviewer", "scout"]}),
        )
    }

    #[test]
    fn ctrl_t_shows_what_the_plugins_wrote_into_the_session() {
        let tree = room(vec![
            posted(2, "itm_1", "reviewer", "what is left?"),
            log_frame(3, tasks()),
            log_frame(4, members()),
        ]);
        let (mut ui, now) = scene();
        ui.panel = true;
        let screen = render_tree(&tree, &ui, now);
        assert!(screen.contains("bingo.tasks · tasks"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn the_panel_shows_the_session_in_view_and_says_when_it_is_empty() {
        let mut tree = room(vec![log_frame(2, tasks())]);
        let (mut ui, now) = scene();
        ui.panel = true;
        assert!(render_tree(&tree, &ui, now).contains("write the plan"));

        let root = tree.root_id().clone();
        tree.show(&root);
        let screen = render_tree(&tree, &ui, now);
        assert!(screen.contains(crate::panel::NOTHING), "{screen}");
        assert!(!screen.contains("write the plan"), "{screen}");
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
        // The move eases over 100 ms; this is the screen it settles on.
        let settled = Now {
            instant: now.instant + crate::scroll::EASE,
            ..now
        };
        let scrolled = render(&state, &ui, settled);
        assert_ne!(bottom, scrolled, "page up must move the window");
        insta::assert_snapshot!(scrolled);
    }
}
