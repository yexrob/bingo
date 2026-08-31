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
use ratatui::widgets::{Block, Clear, Padding, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::clock::{self, Now};
use crate::frame::{self, Demand, Regions};
use crate::tree::{self, Tree};
use crate::ui::{Card, Open, Picker, Switcher, Ui};
use crate::{
    composer as prompt, dialog, keys, layers, pager, panel, rail, rewind, search, select, status,
    theme, views, wrap,
};

/// How tall the composer box may grow before it scrolls internally.
const COMPOSER_ROWS: usize = 10;
/// How many dropdown rows are shown at once.
const MENU_ROWS: usize = 8;
/// How long a turn must have run before it is worth a row of its own (§6).
const ACTIVITY_AFTER: std::time::Duration = std::time::Duration::from_millis(300);
/// One breath of bingo's presence: the sparkle and the box's border (§6).
const BREATH: std::time::Duration = std::time::Duration::from_millis(1600);
/// One turn of the sparkle: four glyphs, 150 ms each (§6).
const SPARKLE: std::time::Duration = std::time::Duration::from_millis(4 * theme::SPARKLE_MS as u64);
/// bingo's own words for working (§4), one per turn.
const VERBS: [&str; 8] = [
    "Simmering",
    "Noodling",
    "Tinkering",
    "Rummaging",
    "Mulling",
    "Weaving",
    "Sketching",
    "Percolating",
];

/// One render path for the whole tree: it paints the session in view and
/// derives everything about the others — the counts on the status line, the
/// `↳` rows, the switcher — from their states.
pub fn draw(tree: &Tree, ui: &Ui, frame: &mut Frame, now: Now) {
    let area = frame.area();
    let cards = rail::cards(tree.viewed(), tree.view(), &ui.pinned);
    let regions = frame::regions(area, demand(tree, ui, area.width, now, !cards.is_empty()));
    ui.painted.borrow_mut().regions = regions;
    let drawn = rail::render(
        &cards,
        rail::width(regions.rail, regions.transcript),
        ui.focus.as_ref(),
        &ui.marks(tree.viewed()),
    );
    // A card is in the rail, or — where there is no rail — under the running
    // rows; never in both (design §3).
    let live = match regions.rail {
        Some(_) => Vec::new(),
        None => rail::inline(&drawn),
    };
    render_transcript(tree, ui, frame, regions.transcript, now, live);
    render_rail(ui, frame, regions.rail, &drawn);
    render_activity(tree.viewed(), frame, regions.activity, now);
    render_composer(tree.viewed(), ui, frame, regions.composer, now);
    render_status(tree, ui, frame, regions.status, now);
    layers(tree, ui, frame, regions, now);
}

/// What the frame must make room for before the transcript is given the rest.
fn demand(tree: &Tree, ui: &Ui, width: u16, now: Now, rail: bool) -> Demand {
    let state = tree.viewed();
    Demand {
        composer: u16::try_from(composer_rows(state, ui, width as usize)).unwrap_or(u16::MAX),
        activity: u16::try_from(activity(state, now).len()).unwrap_or(u16::MAX),
        rail,
    }
}

/// The rail: the cards on their raised tint, from the top row down. Where
/// each landed is left in [`crate::ui::Painted`] for a click to read.
fn render_rail(ui: &Ui, frame: &mut Frame, area: Option<Rect>, drawn: &[rail::Drawn]) {
    let Some(area) = area.filter(|area| area.height > 0) else {
        ui.painted.borrow_mut().rail.clear();
        return;
    };
    let (lines, where_) = rail::painted(drawn, area.width as usize);
    ui.painted.borrow_mut().rail = where_;
    frame.render_widget(Paragraph::new(lines), area);
}

/// The rows the draft needs, at most [`COMPOSER_ROWS`].
fn composer_rows(state: &SessionState, ui: &Ui, width: usize) -> usize {
    ui.composer
        .layout(inner_width(state, width))
        .lines
        .len()
        .clamp(1, COMPOSER_ROWS)
}

/// The cells inside the box: two border columns, a cell of padding each
/// side, then the prompt itself (`> `, or `#design > ` in a room).
fn inner_width(state: &SessionState, width: usize) -> usize {
    width
        .saturating_sub(4 + prompt::prompt(state).width())
        .max(1)
}

/// The status line, or the search row in its place while one is open.
fn render_status(tree: &Tree, ui: &Ui, frame: &mut Frame, area: Rect, now: Now) {
    if area.height == 0 {
        return;
    }
    let line = match ui.search.as_ref() {
        Some(search) => search::row(search),
        None => status::line(tree, ui, area.width as usize, now),
    };
    frame.render_widget(Paragraph::new(vec![line]), area);
}

/// The activity row and whatever is queued behind it.
fn render_activity(state: &SessionState, frame: &mut Frame, area: Rect, now: Now) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(Paragraph::new(activity(state, now)), area);
}

/// What floats over the frame, in the order it is stacked: the dropdown and a
/// command's block ride just above the input box; a card or a sheet dims the
/// world and takes the screen. Every one of them is a layer, not a row — the
/// input box never moves to make room.
fn layers(tree: &Tree, ui: &Ui, frame: &mut Frame, regions: Regions, now: Now) {
    let width = regions.transcript.width as usize;
    let above = regions.above();
    over(
        frame,
        above,
        [
            ui.block
                .as_ref()
                .map(|view| views::render(view, width))
                .unwrap_or_default(),
            menu(ui, &tree.viewed().summary.cwd),
        ]
        .concat(),
    );
    ui.painted.borrow_mut().card = None;
    match ui.layer.drawn(now) {
        Some(reveal) => layer(tree, ui, frame, above, reveal, width, now),
        None => card(tree, ui, frame, regions, now),
    }
}

/// The layer a person opened: a sheet over the whole of the frame, or the
/// switcher's card above the input box.
fn layer(
    tree: &Tree,
    ui: &Ui,
    frame: &mut Frame,
    above: Rect,
    reveal: layers::Reveal,
    width: usize,
    now: Now,
) {
    match &ui.layer.open {
        Open::Nothing => {}
        // A dropdown above the input box, like the `/` menu: nothing dims.
        Open::Switcher(switcher) => over(frame, above, switcher_lines(tree, switcher, now)),
        Open::Help => sheet(frame, above, help(ui, width), reveal),
        Open::Panel => sheet(
            frame,
            above,
            panel::lines(tree.viewed(), tree.view(), ui.panel, &ui.pinned, width),
            reveal,
        ),
        Open::Picker(picker) => sheet(frame, above, picker_lines(picker), reveal),
        Open::Pager(open) => paged(tree, frame, above, open, reveal),
        // A dropdown above the input box, like the switcher's.
        Open::Rewind(card) => over(
            frame,
            above,
            rewind::lines(&rewind::turns(tree.viewed()), card.selected),
        ),
    }
}

/// One block, whole. What it shows is read from the item every frame, so a
/// block that is still arriving grows under the sheet rather than being copied
/// into it.
fn paged(
    tree: &Tree,
    frame: &mut Frame,
    above: Rect,
    open: &crate::pager::Pager,
    reveal: layers::Reveal,
) {
    let Some(item) = tree.viewed().items.iter().find(|item| item.id == open.item) else {
        return;
    };
    let content = pager::lines(item, above.width as usize);
    let window = pager::Window {
        height: content.len(),
        rows: usize::from(above.height).saturating_sub(pager::HEAD),
    };
    let lines = pager::sheet(&pager::title(item), &content, open, window);
    sheet(frame, above, lines, reveal);
    marked(frame, above, open, window, reveal);
}

/// The hits of the pager's own query, once the sheet has finished arriving:
/// one still sliding up would carry the marks to the wrong rows.
fn marked(
    frame: &mut Frame,
    above: Rect,
    open: &crate::pager::Pager,
    window: pager::Window,
    reveal: layers::Reveal,
) {
    let Some(search) = open.search.as_ref().filter(|_| !reveal.moving()) else {
        return;
    };
    let head = u16::try_from(pager::HEAD).unwrap_or(u16::MAX);
    let area = Rect {
        y: above.y + head,
        height: above.height.saturating_sub(head),
        ..above
    };
    search::mark(frame, area, open.at(window), search);
}

/// A sheet over a dimmed frame.
fn sheet(frame: &mut Frame, above: Rect, lines: Vec<Line<'static>>, reveal: layers::Reveal) {
    layers::dim(frame);
    layers::sheet(frame, above, lines, reveal);
}

/// The open interaction, as a bordered box under the `⎿` of the row that
/// asked. Its arrival is measured from when the kernel opened it, so a card
/// that was already up when this client attached is simply there.
fn card(tree: &Tree, ui: &Ui, frame: &mut Frame, regions: Regions, now: Now) {
    let Some((owner, interaction)) = tree.open_interaction() else {
        return;
    };
    let asked_elsewhere = owner.summary.id != *tree.view();
    let agent = asked_elsewhere.then(|| tree::name(owner));
    // Each row keeps the option it belongs to through the wrap, so a click
    // lands on what the eye is on. Two border cells and two of padding.
    let width = regions.transcript.width.saturating_sub(4) as usize;
    let rows: Vec<(Line<'static>, Option<usize>)> = dialog::rows(
        &ui.dialog,
        interaction,
        agent.as_deref(),
        &owner.summary.cwd,
    )
    .into_iter()
    .flat_map(|(line, option)| {
        wrap::wrap_all(std::slice::from_ref(&line), width)
            .into_iter()
            .map(move |wrapped| (wrapped, option))
    })
    .collect();
    let lines: Vec<Line<'static>> = rows.iter().map(|(line, _)| line.clone()).collect();
    let above = regions.above();
    // Only a row of the transcript on screen can anchor it: a child's item
    // ids are its own, and would name the wrong row here.
    let anchor = (!asked_elsewhere)
        .then(|| asking_row(ui, interaction, regions))
        .flatten();
    let at = layers::under(above, anchor, rows_of(&lines, above));
    ui.painted.borrow_mut().card = Some(Card {
        area: at,
        options: rows.iter().map(|(_, option)| *option).collect(),
    });
    layers::dim(frame);
    layers::card(
        frame,
        at,
        lines,
        opening(interaction, now),
        guarded(interaction, now),
    );
}

/// Whether the kernel's guard is still down. A card that cannot be answered
/// yet says so by being dim: the keys a person presses now would be dropped,
/// and dropping them silently is what makes a guard feel like a bug (§6).
fn guarded(interaction: &bingo_sdk::Interaction, now: Now) -> bool {
    interaction
        .guard_until
        .is_some_and(|until| !now.reached(until))
}

/// Where the row that asked ends, in screen rows, when it is on the screen.
fn asking_row(ui: &Ui, interaction: &bingo_sdk::Interaction, regions: Regions) -> Option<u16> {
    let painted = ui.painted.borrow();
    let region = regions.transcript;
    let line = painted.blocks.span(interaction.item.as_ref()?)?.1;
    // A short transcript hangs from the foot of its region, so the rows above
    // it are padding the line numbers know nothing about.
    let rows = usize::from(region.height);
    let padding = rows - painted.height.min(rows);
    let row = u16::try_from(line.checked_sub(painted.top)? + padding).ok()?;
    (row < region.height).then(|| region.y + row)
}

/// How far into its arrival a card the kernel opened is. A still surface has
/// it whole from the first frame.
fn opening(interaction: &bingo_sdk::Interaction, now: Now) -> layers::Reveal {
    if !now.motion {
        return layers::Reveal::whole(layers::CARD_FRAMES);
    }
    let elapsed = now.past(interaction.opened_at);
    let since = now.instant.checked_sub(elapsed).unwrap_or(now.instant);
    layers::Reveal::at(layers::CARD_FRAMES, since, now.instant, false)
}

fn rows_of(lines: &[Line<'static>], region: Rect) -> u16 {
    u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(region.height)
}

/// Rows that ride just above the input box, trimmed from the top when the
/// region cannot hold them all.
fn over(frame: &mut Frame, region: Rect, lines: Vec<Line<'static>>) {
    if lines.is_empty() {
        return;
    }
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .min(region.height);
    if height == 0 {
        return;
    }
    let dropped = lines.len() - height as usize;
    let area = Rect {
        y: region.bottom() - height,
        height,
        ..region
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines[dropped..].to_vec()), area);
}

/// The tail of the transcript, or the window the scroll keys parked on. What
/// it drew is left in [`crate::ui::Painted`] for the next key to read.
fn render_transcript(
    tree: &Tree,
    ui: &Ui,
    frame: &mut Frame,
    area: Rect,
    now: Now,
    live: Vec<Line<'static>>,
) {
    if area.height == 0 {
        return;
    }
    let rows = area.height as usize;
    let mut painted = ui.painted.borrow_mut();
    painted.height = painted.blocks.sync(
        tree.viewed(),
        &tree.agents(),
        area.width as usize,
        &ui.expanded,
        live,
        now,
    );
    painted.top = ui.scroll.top(painted.height, rows, now.instant);
    let mut shown = painted.blocks.window(painted.top, rows);
    // A short transcript hangs from the composer, not from the top of the screen.
    let padding = rows - shown.len();
    shown.splice(..0, std::iter::repeat_n(Line::default(), padding));
    frame.render_widget(Paragraph::new(shown), area);
    let top = painted.top.saturating_sub(padding);
    if let Some(search) = ui.search.as_ref() {
        search::mark(frame, area, top, search);
    }
    if let Some(run) = ui.select.run.as_ref() {
        select::mark(frame, area, top, run);
    }
    if ui.crossfading(now) {
        layers::hush(frame, area);
    }
}

/// The rows between the transcript and the input box: what the turn is doing,
/// and whatever is queued behind it.
fn activity(state: &SessionState, now: Now) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = working(state, now).into_iter().collect();
    out.extend(state.queue.iter().map(|entry| {
        Line::from(Span::styled(
            format!("{} {}", theme::user(), entry.preview),
            theme::dim(),
        ))
    }));
    // A blank row between the transcript and these, as between any two blocks
    // (§3): they are not the tail of what was said, they are what is going on.
    if !out.is_empty() {
        out.insert(0, Line::default());
    }
    out
}

/// `✻ Simmering… (esc to interrupt · 4s · ↓ 1.2k tokens)` — but only once the
/// turn has been at it for [`ACTIVITY_AFTER`]: a turn that answers at once
/// says nothing at all, because a row that flashes reports nothing (§6).
fn working(state: &SessionState, now: Now) -> Option<Line<'static>> {
    let turn = state.turn.as_ref()?;
    let elapsed = now.past(turn.started_at);
    if elapsed < ACTIVITY_AFTER {
        return None;
    }
    let mut spans = vec![
        Span::styled(format!("{} ", sparkle(now)), breathing(now)),
        Span::styled(
            format!("{}{}", verb(&turn.id), theme::ellipsis()),
            theme::text(),
        ),
        Span::styled(
            format!(
                " (esc to interrupt · {}s{})",
                elapsed.as_secs(),
                spent(turn)
            ),
            theme::dim(),
        ),
    ];
    if let Some(retry) = turn.retrying {
        spans.push(Span::styled(
            format!(" retrying {}/{}", retry.attempt, retry.max),
            theme::presence(),
        ));
    }
    Some(Line::from(spans))
}

/// What the turn has said so far, in the thousands §6 writes it in — and
/// nothing at all before it has said anything.
fn spent(turn: &LiveTurn) -> String {
    match turn.usage.output_tokens {
        0 => String::new(),
        tokens => format!(" · ↓ {:.1}k tokens", tokens as f64 / 1000.0),
    }
}

/// bingo's own word for what it is doing (§4), drawn once per turn from the
/// turn's own id — so the same turn always reads the same way and a test can
/// name what it will say.
fn verb(turn: &bingo_sdk::TurnId) -> &'static str {
    VERBS[seed(turn.as_str()) % VERBS.len()]
}

/// FNV-1a: a stable spread over the words without a dependency to make one.
fn seed(id: &str) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash as usize
}

/// The sparkle's frame, or its first one when nothing may move.
fn sparkle(now: Now) -> &'static str {
    match now.motion {
        true => theme::sparkle(clock::cycle(now, SPARKLE)),
        false => theme::spark(),
    }
}

/// bingo breathing: the sparkle and the input box's border share one clock,
/// so the whole surface inhales together. Still, it rests at `presence` —
/// what breathes is the brightness, not the fact that it is working.
fn breathing(now: Now) -> ratatui::style::Style {
    match now.motion {
        true => theme::breath(clock::breath(now, BREATH)),
        false => theme::presence(),
    }
}

/// The `?` panel: the one binding table, then the commands this session can
/// run — the surface's own and the kernel's, from the same list the dropdown
/// ranks.
fn help(ui: &Ui, width: usize) -> Vec<Line<'static>> {
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

/// The `ctrl+g` list: the root and its agents, with what each is doing.
fn switcher_lines(tree: &Tree, switcher: &Switcher, now: Now) -> Vec<Line<'static>> {
    tree::switcher_lines(&tree.rows(), switcher.selected, now)
}

fn picker_lines(picker: &Picker) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled(
        "Resume".to_string(),
        theme::bold(),
    ))];
    for (index, session) in picker.sessions.iter().enumerate() {
        let selected = index == picker.selected;
        let style = if selected {
            theme::text()
        } else {
            theme::dim()
        };
        let title = session.title.clone().unwrap_or_else(|| "untitled".into());
        out.push(Line::from(vec![
            theme::cursor_span(selected),
            Span::styled(
                format!(
                    "{}. {title} · {} · {}",
                    index + 1,
                    session.updated_at,
                    session.id
                ),
                style,
            ),
        ]));
    }
    out
}

/// The prompt box: the caret lives here and nowhere else. Its border is the
/// one box the frame draws itself; what is inside it is [`crate::composer`]'s.
fn render_composer(state: &SessionState, ui: &Ui, frame: &mut Frame, area: Rect, now: Now) {
    if area.height == 0 {
        return;
    }
    let block = Block::bordered()
        .border_set(theme::border())
        .border_style(border(state, now))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let prompt = prompt::prompt(state);
    let layout = ui.composer.layout(inner_width(state, area.width as usize));
    // Scroll only as far as the caret needs: it must stay in the box.
    let start = layout.cursor.0.saturating_sub(COMPOSER_ROWS - 1);
    let placeholder = placeholder(state);
    let lines = prompt::box_lines(
        &layout,
        &prompt,
        (start, COMPOSER_ROWS),
        ui.composer.is_empty().then_some(placeholder.as_str()),
    );
    frame.render_widget(Paragraph::new(lines), inner);
    frame.set_cursor_position((
        inner.x
            + u16::try_from(layout.cursor.1 + prompt.width())
                .unwrap_or(u16::MAX)
                .min(inner.width.saturating_sub(1)),
        inner.y
            + u16::try_from(layout.cursor.0 - start)
                .unwrap_or(u16::MAX)
                .min(inner.height.saturating_sub(1)),
    ));
}

/// The box's border: `dim` while nothing is happening, and glowing on the
/// activity row's own breath while the model works (§4).
fn border(state: &SessionState, now: Now) -> ratatui::style::Style {
    match state.busy() {
        true => breathing(now),
        false => theme::dim(),
    }
}

/// What the empty composer offers. Nothing answers a `Log` session, so it is
/// posted into rather than asked (ADR-0011 §1) — and the prompt already says
/// which room, so the placeholder does not say it twice.
fn placeholder(state: &SessionState) -> String {
    match state.summary.driver {
        Driver::Log => "post to the room".to_string(),
        Driver::Model => keys::PLACEHOLDER.to_string(),
    }
}

fn menu(ui: &Ui, cwd: &str) -> Vec<Line<'static>> {
    let rows = ui.suggestions(cwd);
    let selected = ui.menu.selected.min(rows.len().saturating_sub(1));
    let column = rows.iter().map(|r| r.label.width()).max().unwrap_or(0);
    rows.iter()
        .enumerate()
        .take(MENU_ROWS)
        .map(|(index, row)| {
            let focused = index == selected;
            let style = if focused { theme::text() } else { theme::dim() };
            let label = format!("{:<column$}", row.label, column = column);
            Line::from(vec![
                theme::cursor_span(focused),
                Span::styled(
                    format!("{label}  {}", row.hint).trim_end().to_string(),
                    style,
                ),
            ])
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
    use crossterm::event::KeyCode;
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
        let (ui, now) = mid_turn();
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
        let (ui, now) = mid_turn();
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
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn permission_expanded() {
        let state = folded(vec![frame(
            1,
            opened(permission(Some("Edit(src/)"), Some(long_diff()))),
        )]);
        let (mut ui, now) = settled();
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
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        crate::input::on_key(&mut ui, &solo(&state), typed('n'), now);
        write(&mut ui, &state, "use cargo clean", now);
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn a_card_comes_down_from_its_top_edge() {
        let asked = folded(vec![frame(1, opened(permission(Some("Edit(src/)"), None)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(asked.interactions.first());
        // Frame 0 is the screen before the kernel opened it; then one frame
        // every 33 ms until the card is whole.
        let mut screens = vec![render(&state(), &ui, now)];
        screens.extend((0..3).map(|f| render(&asked, &ui, later(now, f * 33))));
        assert_eq!(
            screens[3],
            render(&asked, &ui, later(now, 500)),
            "by the third frame it has arrived"
        );
        insta::assert_snapshot!(screens.join("\n"));
    }

    #[test]
    fn a_card_hangs_under_the_row_that_asked_for_it() {
        // The permission fixture names `itm_2` as the row that asked; the
        // rows after it are what the card comes down over.
        let mut frames = vec![
            item_frame(1, user("itm_1", "edit it")),
            item_frame(
                2,
                tool(
                    "itm_2",
                    "Edit",
                    json!({"file_path": "src/lib.rs"}),
                    None,
                    ItemStatus::Running,
                ),
            ),
        ];
        frames.extend((3..12).map(|i| {
            item_frame(
                i,
                assistant(
                    &format!("itm_{i}"),
                    &format!("after {i}"),
                    ItemStatus::Completed,
                ),
            )
        }));
        let state = folded(frames);
        let mut state = state.clone();
        state.apply(&frame(12, opened(permission(Some("Edit(src/)"), None))));
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        let screen = render(&state, &ui, now);
        let rows: Vec<&str> = screen.lines().collect();
        let asked = rows
            .iter()
            .position(|row| row.contains("Edit(src/lib.rs)"))
            .expect("the row that asked");
        let card = rows
            .iter()
            .position(|row| row.contains('╭'))
            .expect("the card's top edge");
        assert_eq!(card, asked + 1, "the box opens under it:\n{screen}");
        assert!(
            rows.last().is_some_and(|row| row.contains("fake-1")),
            "and the frame under it did not move:\n{screen}"
        );
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn everything_behind_a_card_is_dim() {
        let state = folded(vec![
            item_frame(1, user("itm_1", "edit it")),
            frame(2, opened(permission(Some("Edit(src/)"), None))),
        ]);
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        let screen = drawn(80, 24, &solo(&state), &ui, now);
        let card: Vec<usize> = screen
            .buffer()
            .content()
            .chunks(80)
            .enumerate()
            .filter(|(_, row)| row.iter().any(|cell| cell.symbol() == "\u{256d}"))
            .map(|(y, _)| y)
            .collect();
        let top = *card.first().expect("a card on the screen");
        for (y, row) in screen.buffer().content().chunks(80).enumerate() {
            if y >= top {
                continue;
            }
            for cell in row {
                assert!(
                    cell.style()
                        .add_modifier
                        .contains(ratatui::style::Modifier::DIM),
                    "row {y} is behind the card and not dim"
                );
            }
        }
    }

    #[test]
    fn question_single() {
        let state = folded(vec![frame(1, opened(question(false, false)))]);
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn question_multi() {
        let state = folded(vec![frame(1, opened(question(true, true)))]);
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        crate::input::on_key(&mut ui, &solo(&state), typed(' '), now);
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn confirm_dialog() {
        let state = folded(vec![frame(1, opened(confirm()))]);
        let (mut ui, now) = settled();
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
        let (mut ui, now) = settled();
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
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn login_paste_dialog_with_the_words_row_open() {
        let state = folded(vec![frame(1, opened(login(bingo_sdk::LoginFlow::Paste)))]);
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        crate::input::on_key(&mut ui, &solo(&state), typed('1'), now);
        write(&mut ui, &state, "sk-pasted-elsewhere", now);
        insta::assert_snapshot!(render(&state, &ui, now));
    }

    #[test]
    fn help_panel() {
        let state = state();
        let (mut ui, now) = scene();
        shown(&mut ui, Open::Help, now);
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
        let (ui, now) = mid_turn();
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
        shown(
            &mut ui,
            Open::Picker(Picker {
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
            }),
            now,
        );
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
        assert!(screen.contains("⏺ reviewer(review the diff)"), "{screen}");
        assert!(screen.contains("⎿  Running…"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn a_child_that_needs_a_person_is_counted_on_the_status_line() {
        let tree = spawned(vec![child_frame(2, opened(child_permission()))]);
        let (mut ui, now) = settled();
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
        let (ui, now) = mid_turn();
        let screen = render_tree(&tree, &ui, now);
        assert!(screen.contains("in reviewer · fake-1"), "{screen}");
        assert!(screen.contains("Two nits"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn the_switcher_lists_the_root_and_its_agents() {
        let tree = spawned(vec![child_frame(2, opened(child_permission()))]);
        let (mut ui, now) = scene();
        shown(&mut ui, Open::Switcher(Switcher { selected: 1 }), now);
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
        assert!(screen.contains("#design > post to the room"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn a_room_sits_in_the_switcher_with_no_status() {
        let mut tree = room(vec![child_frame(1, announced("reviewer"))]);
        let root = tree.root_id().clone();
        tree.show(&root);
        let (mut ui, now) = scene();
        shown(&mut ui, Open::Switcher(Switcher { selected: 1 }), now);
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
        shown(&mut ui, Open::Panel, now);
        let screen = render_tree(&tree, &ui, now);
        assert!(screen.contains("bingo.tasks · tasks"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn the_panel_shows_the_session_in_view_and_says_when_it_is_empty() {
        let mut tree = room(vec![log_frame(2, tasks())]);
        let (mut ui, now) = scene();
        shown(&mut ui, Open::Panel, now);
        assert!(render_tree(&tree, &ui, now).contains("write the plan"));

        let root = tree.root_id().clone();
        tree.show(&root);
        let screen = render_tree(&tree, &ui, now);
        assert!(screen.contains(crate::panel::NOTHING), "{screen}");
        assert!(!screen.contains("write the plan"), "{screen}");
    }

    // ---- the three lanes (ADR-0013) -------------------------------------

    /// A person who pinned the board into the rail and put the focus on it.
    fn watching() -> (Ui, Now) {
        let (mut ui, now) = scene();
        pin_board(&mut ui);
        ui.focus = Some(demo_card("board"));
        (ui, now)
    }

    #[test]
    fn the_rail_holds_the_pinned_board_and_the_live_progress_card() {
        let (ui, now) = watching();
        let screen = draw_sized(120, 40, &boarded(), &ui, now);
        assert!(screen.contains("❯ Board"), "{screen}");
        assert!(screen.contains("[ 1 Tick ]"), "{screen}");
        assert!(screen.contains("████████░░ 80 %"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn a_rail_is_not_drawn_for_a_session_no_plugin_has_written_to() {
        let (ui, now) = scene();
        draw_sized(120, 40, &state(), &ui, now);
        let quiet = ui.painted.borrow().regions;
        assert!(quiet.rail.is_none(), "an empty rail is not drawn");
        assert_eq!(quiet.transcript.width, 120, "the transcript keeps it all");

        let (ui, now) = watching();
        draw_sized(120, 40, &boarded(), &ui, now);
        let busy = ui.painted.borrow().regions;
        assert!(busy.rail.is_some(), "a card asks for the column");
        assert_eq!(busy.transcript.width, 120 - crate::frame::RAIL_WIDTH);
    }

    /// Below the rail's width the same cards draw under the running rows, so
    /// a signal is never lost to a narrow terminal (design §3).
    #[test]
    fn without_a_rail_the_same_cards_draw_under_the_running_rows() {
        let (ui, now) = watching();
        let screen = render(&boarded(), &ui, now);
        assert!(screen.contains("████████░░ 80 %"), "{screen}");
        assert!(screen.contains("❯ Board"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    /// The block lane: what a tool drew for a person, under its own row and
    /// folded like any other output.
    #[test]
    fn a_display_view_is_drawn_under_the_tool_row_that_made_it() {
        let output = ToolOutput {
            parts: vec![ContentPart::text("2 open")],
            is_error: false,
            display: Some(View::Table {
                headers: vec!["id".into(), "task".into()],
                rows: vec![
                    vec!["1".into(), "write the plan".into()],
                    vec!["2".into(), "ship it".into()],
                ],
            }),
        };
        let state = folded(vec![item_frame(
            1,
            tool(
                "itm_1",
                "TaskList",
                json!({}),
                Some(output),
                ItemStatus::Completed,
            ),
        )]);
        let (ui, now) = scene();
        let screen = render(&state, &ui, now);
        assert!(screen.contains("⎿  id  task"), "{screen}");
        assert!(!screen.contains("2 open"), "the model's text is not drawn");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn the_composer_survives_a_screen_too_small_for_the_chrome() {
        let state = folded(vec![frame(
            1,
            opened(permission(Some("E(s/)"), Some(long_diff()))),
        )]);
        let (mut ui, now) = scene();
        shown(&mut ui, Open::Help, now);
        ui.dialog.focus_on(state.interactions.first());
        let screen = draw_sized(60, 12, &state, &ui, now);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[rows.len() - 4].contains('\u{256d}'), "{screen}");
        assert!(rows[rows.len() - 3].contains("│ > "), "{screen}");
        assert!(rows[rows.len() - 2].contains('\u{256f}'), "{screen}");
        assert!(rows[rows.len() - 1].contains("? for shortcuts"), "{screen}");
    }

    /// §6's budget is for the binary a person runs, which is built in
    /// release; the tests run in debug, where the same draw is about four
    /// times slower, so debug is held to four times the budget and release to
    /// the budget itself.
    #[test]
    fn a_full_draw_of_a_long_transcript_is_inside_the_frame_budget() {
        let state = long_transcript(5_000);
        let (ui, now) = scene();
        draw_sized(120, 40, &state, &ui, now);
        let started = std::time::Instant::now();
        let draws = 20;
        for _ in 0..draws {
            draw_sized(120, 40, &state, &ui, now);
        }
        let each = started.elapsed() / draws;
        let (budget, profile) = match cfg!(debug_assertions) {
            true => (std::time::Duration::from_millis(16), "debug"),
            false => (std::time::Duration::from_millis(4), "release"),
        };
        assert!(
            each < budget,
            "a warm draw at 120x40 took {each:?} ({profile})"
        );
    }

    #[test]
    fn a_terminal_too_small_for_anything_still_draws() {
        let (ui, now) = scene();
        for (width, height) in [(1u16, 1u16), (4, 2), (10, 3), (20, 5)] {
            draw_sized(width, height, &state(), &ui, now);
        }
    }

    #[test]
    fn ctrl_f_searches_the_transcript_and_esc_gives_the_status_line_back() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        let press = |ui: &mut Ui, key| {
            crate::input::on_key(ui, &solo(&state), key, now);
        };
        press(&mut ui, ctrl('f'));
        for c in "line 4".chars() {
            press(&mut ui, typed(c));
        }
        let typing = render(&state, &ui, now);
        assert!(typing.contains("/line 4▌"), "{typing}");

        press(&mut ui, key(crossterm::event::KeyCode::Enter));
        // The transcript eases to the hit; this is where it lands.
        let there = Now {
            instant: now.instant + crate::scroll::EASE,
            ..now
        };
        let found = render(&state, &ui, there);
        assert!(found.contains("1/11 · n/N · esc"), "{found}");
        assert!(found.contains("line 4"), "it scrolled to the hit: {found}");
        insta::assert_snapshot!(found);

        press(&mut ui, typed('n'));
        let stepped = render(&state, &ui, there);
        assert!(stepped.contains("2/11"), "{stepped}");

        press(&mut ui, key(crossterm::event::KeyCode::Esc));
        assert!(
            render(&state, &ui, there).contains("? for shortcuts"),
            "esc gives the status line back"
        );
    }

    #[test]
    fn a_hit_on_the_screen_is_marked_where_it_sits() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        crate::input::on_key(&mut ui, &solo(&state), ctrl('f'), now);
        for c in "line 59".chars() {
            crate::input::on_key(&mut ui, &solo(&state), typed(c), now);
        }
        crate::input::on_key(
            &mut ui,
            &solo(&state),
            key(crossterm::event::KeyCode::Enter),
            now,
        );
        let screen = drawn(
            80,
            24,
            &solo(&state),
            &ui,
            Now {
                instant: now.instant + crate::scroll::EASE,
                ..now
            },
        );
        let marked: Vec<(u16, u16)> = screen
            .buffer()
            .content()
            .iter()
            .enumerate()
            .filter(|(_, cell)| {
                cell.style() == theme::as_drawn(theme::presence().patch(theme::bold()))
            })
            .map(|(i, _)| ((i % 80) as u16, (i / 80) as u16))
            .collect();
        assert_eq!(marked.len(), 7, "seven cells of `line 59`: {marked:?}");
        assert!(
            marked.iter().all(|(x, _)| (2..9).contains(x)),
            "after the `❯ `: {marked:?}"
        );
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

    /// `ctrl+o` only opens further: the fold lifts, then the whole of it takes
    /// the sheet, and `esc` there folds it back (M11e reverses M11a's
    /// "again to fold" on purpose).
    #[test]
    fn ctrl_o_lifts_the_fold_then_opens_the_pager_and_esc_folds_it_back() {
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
        let (mut ui, now) = scene();
        assert!(render(&state, &ui, now).contains("+4 lines (ctrl+o to expand)"));
        let tree = solo(&state);
        crate::input::on_key(&mut ui, &tree, ctrl('o'), now);
        let opened = render(&state, &ui, now);
        assert!(opened.contains("line 9"), "{opened}");
        assert!(!opened.contains("+4 lines"), "{opened}");

        crate::input::on_key(&mut ui, &tree, ctrl('o'), now);
        let paged = render(&state, &ui, later(now, 200));
        assert!(
            paged.contains("Read(src/lib.rs)"),
            "the sheet says what it is: {paged}"
        );
        assert!(paged.contains("j/k"), "{paged}");

        crate::input::on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(
            render(&state, &ui, later(now, 200)).contains("+4 lines (ctrl+o to expand)"),
            "leaving the sheet folds the result it came from"
        );
    }

    #[test]
    fn the_pager_scrolls_searches_and_leaves_the_frame_as_it_found_it() {
        let output = ToolOutput {
            parts: vec![ContentPart::text(
                (1..=40).map(|i| format!("line {i}\n")).collect::<String>(),
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
        let (mut ui, now) = scene();
        let tree = solo(&state);
        let before = render(&state, &ui, now);

        // A click focuses the block; `⏎` opens it whole.
        crate::input::on_key(&mut ui, &tree, ctrl('o'), now);
        crate::input::on_key(&mut ui, &tree, ctrl('o'), now);
        let settled = later(now, 200);
        let top = render(&state, &ui, settled);
        assert!(top.contains("line 1"), "{top}");

        crate::input::on_key(&mut ui, &tree, typed('G'), now);
        let bottom = render(&state, &ui, settled);
        assert!(bottom.contains("line 40"), "{bottom}");
        assert_ne!(top, bottom, "G takes the end of it");

        crate::input::on_key(&mut ui, &tree, typed('/'), now);
        for c in "line 7".chars() {
            crate::input::on_key(&mut ui, &tree, typed(c), now);
        }
        let typing = render(&state, &ui, settled);
        assert!(typing.contains("/line 7"), "{typing}");
        crate::input::on_key(&mut ui, &tree, key(KeyCode::Enter), now);
        let found = render(&state, &ui, settled);
        assert!(found.contains("1/1 · n/N · esc"), "{found}");
        assert!(found.contains("line 7"), "{found}");

        crate::input::on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        crate::input::on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert_eq!(
            render(&state, &ui, later(now, 400)),
            before,
            "the frame beneath is what it was"
        );
    }
}
