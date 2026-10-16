//! The header, transcript, rule, prompt, hints and footer in one frame.
//! `compose` fills an owned layout without borrowing a terminal frame.

pub(crate) mod header;

use crate::layout::Layout;
use bingo_sdk::SessionState;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::clock::Now;
use crate::commands::{Group, Suggestion};
use crate::composer::strip;
use crate::frame::{self, Demand, Regions};
use crate::tree::{self, Tree};
use crate::ui::{Card, Open, Ui};
use crate::{
    activity, composer as prompt, dialog, foot, graphics, layers, mentions, pager, rewind, search,
    select, status, theme, transcript, views, window, wrap,
};

/// How tall the composer box may grow before it scrolls internally.
const COMPOSER_ROWS: usize = 10;
/// How many dropdown rows are shown at once.
const MENU_ROWS: usize = 8;

/// One render path for the whole tree: it paints the session in view and
/// derives everything about the others — the counts on the status line, the
/// `↳` rows, the switcher — from their states.
pub fn compose(tree: &Tree, ui: &Ui, frame: &mut Layout, now: Now) {
    let area = frame.area();
    let state = tree.viewed();
    let draft = ui.composer.layout(inner_width(state, area.width as usize));
    let strip = strip(state, ui, &frame.measurements, area.width);
    let mut activity = activity::components(tree, ui, area.width as usize, now, &mut frame.pulls);
    let band_offset = activity.len();
    let band = crate::component::band::draw(state, ui, frame);
    activity.extend(band.lines.iter().cloned());
    let palette = palette(tree, ui, frame, now);
    let mut regions = frame::regions(area, demand(&draft, &strip, &activity, &palette));
    frame.seating = crate::pane::seating(ui, state, regions.transcript);
    regions.transcript = frame.seating.transcript;
    pull_activity(state, ui, frame, regions.activity);
    let layer_regions = regions;
    if crate::browse::active(ui) && regions.transcript.height > 0 {
        regions.transcript.y += 1;
        regions.transcript.height -= 1;
    }
    frame.regions = regions;
    header::render(tree.viewed(), ui, frame, regions.header, now);
    render_transcript(tree, ui, frame, regions.transcript, now);
    render_activity(ui, frame, regions.activity, activity);
    crate::component::band::place(&band, frame, regions.activity, band_offset);
    render_strip(frame, regions.strip, strip);
    render_composer(tree.viewed(), ui, frame, regions.composer, now, draft);
    crate::pane::draw(ui, state, frame, now);
    render_status(tree, ui, frame, regions.status, now);
    frame.render_widget(Paragraph::new(palette), regions.palette);
    if matches!(ui.focus, crate::focus::Focus::Pane(_)) {
        frame.cursor = None;
    }
    if regions.activity.height == 0 && !ui.scroll.following() {
        let area = Rect::new(
            regions.transcript.x,
            regions.transcript.bottom().saturating_sub(1),
            regions.transcript.width,
            1,
        );
        frame.render_widget(Clear, area);
        render_activity(ui, frame, area, Vec::new());
    }
    layers(tree, ui, frame, layer_regions, now);
    let hints = crate::keybar::draw(tree, ui, frame, now);
    frame.render_widget(Paragraph::new(hints), regions.hints);
}

fn pull_activity(state: &SessionState, ui: &Ui, frame: &mut Layout, area: Rect) {
    if !ui.activity_hidden {
        for placed in bingo_sdk::slot::placed(state) {
            if let bingo_sdk::slot::Slot::Activity {
                view: bingo_sdk::slot::Shown::Pull { request },
                ..
            } = placed.slot
            {
                frame.pulls.push(crate::pulled::Key::new(
                    state,
                    &request.on(bingo_sdk::render::SurfaceName::Terminal),
                    area.width,
                    area.height,
                ));
            }
        }
    }
}

/// What the frame must make room for before the transcript is given the rest.
fn demand(
    draft: &prompt::Layout,
    strip: &graphics::Band,
    activity: &[Line<'_>],
    palette: &[Line<'_>],
) -> Demand {
    Demand {
        composer: u16::try_from(draft.lines.len().clamp(1, COMPOSER_ROWS)).unwrap_or(u16::MAX),
        strip: strip.height(),
        activity: u16::try_from(activity.len()).unwrap_or(u16::MAX),
        palette: u16::try_from(palette.len()).unwrap_or(u16::MAX),
    }
}

/// The open palette's rows, drawn before the frame is cut since they decide
/// how far the box moves up.
fn palette(tree: &Tree, ui: &Ui, frame: &mut Layout, now: Now) -> Vec<Line<'static>> {
    if !crate::palette::active(ui) {
        return Vec::new();
    }
    let (width, rows) = crate::palette::room(frame.area());
    crate::palette::draw(ui, tree, frame, width, rows, now)
}

/// The draft's thumbnails, measured and rendered once for this frame.
fn strip(
    state: &SessionState,
    ui: &Ui,
    measured: &graphics::Decoded,
    width: u16,
) -> graphics::Band {
    strip::rows(
        &ui.pictures,
        ui.composer.text(),
        &ui.linked,
        graphics::chosen(),
        measured,
        u16::try_from(inner_width(state, usize::from(width))).unwrap_or(u16::MAX),
    )
}

/// The text measure after the prompt tag.
fn inner_width(state: &SessionState, width: usize) -> usize {
    width.saturating_sub(prompt::prompt(state).width()).max(1)
}

/// Persistent session facts keep their row in every focus state.
fn render_status(tree: &Tree, ui: &Ui, frame: &mut Layout, area: Rect, now: Now) {
    if area.height == 0 {
        return;
    }
    let area = crate::pane::status(ui, tree.viewed(), frame, area);
    let (line, crew) = status::components(tree, ui, area.width as usize, now, &mut frame.pulls);
    frame.crew = crew.map(|columns| {
        Rect::new(
            area.x + columns.start as u16,
            area.y,
            columns.len() as u16,
            1,
        )
    });
    frame.render_widget(Paragraph::new(vec![line]), area);
}

/// The activity row and whatever is queued behind it. It takes the tree
/// rather than the session on screen: between turns the row says what the
/// sessions *around* this one are still doing (M82).
fn render_activity(ui: &Ui, frame: &mut Layout, area: Rect, mut lines: Vec<Line<'static>>) {
    if area.height == 0 {
        return;
    }
    let below = (!ui.scroll.following()).then(|| {
        frame
            .height
            .saturating_sub(frame.top + usize::from(frame.regions.transcript.height))
    });
    if let Some(pill) = below.and_then(|below| foot::pill(below, area.width as usize)) {
        frame.foot = Some(Rect::new(area.x + pill.x, area.y, pill.width, 1));
        match lines.first_mut() {
            Some(air) => *air = pill.line,
            None => lines.push(pill.line),
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// What floats over the frame, in the order it is stacked: the dropdown and a
/// command's block ride just above the input box; a card or a sheet dims the
/// world and takes the screen. Every one of them is a layer, not a row — the
/// input box never moves to make room.
fn layers(tree: &Tree, ui: &Ui, frame: &mut Layout, regions: Regions, now: Now) {
    let width = regions.transcript.width as usize;
    let above = regions.above();
    over(
        frame,
        above,
        [
            ui.block
                .as_ref()
                .map(|view| views::marked(view, width, &views::Marks::at(now)))
                .unwrap_or_default(),
            menu(
                ui,
                &tree.viewed().summary.cwd,
                &mentions::targets(tree),
                MENU_ROWS.min(usize::from(above.height)),
            ),
        ]
        .concat(),
    );
    match ui.focus.drawn(now) {
        Some(reveal) => layer(tree, ui, frame, above, reveal, width, now),
        None if !crate::ask::drawing::inline(tree, ui) => card(tree, ui, frame, regions, now),
        None => {}
    }
}

/// The layer a person opened: a sheet over the whole of the frame, or the
/// switcher's card above the input box.
fn layer(
    tree: &Tree,
    ui: &Ui,
    frame: &mut Layout,
    above: Rect,
    reveal: layers::Reveal,
    _width: usize,
    _now: Now,
) {
    // Every list a cursor walks is drawn in the room the layer has, and no
    // more of it than that: what is off the window's ends says so (§3).
    let rows = usize::from(above.height);
    match ui.focus.open() {
        Open::Nothing => {}
        Open::Sessions(_) => {
            let lines = crate::sessions::draw(ui, tree, frame, above.width, above.height, _now);
            over(frame, above, lines);
        }
        Open::Browse(_) => {
            let lines = crate::browse::draw(
                ui,
                tree,
                frame,
                above.width,
                above.height.min(frame.canvas.area().height / 2),
            );
            let bar = ratatui::layout::Rect { height: 1, ..above };
            frame.render_widget(
                Paragraph::new(lines.first().cloned().unwrap_or_default()),
                bar,
            );
            if lines.len() > 1 {
                over(frame, above, lines.into_iter().skip(1).collect());
            }
        }
        Open::Plugins(_) => {
            let lines = crate::plugins::draw(ui, tree, frame, above.width, above.height);
            over(frame, above, lines);
        }
        // Drawn under the box, in its own rows (ADR-0095 §1).
        Open::Palette(_) => {}
        Open::Pager(open) => paged(tree, frame, above, open, reveal),
        // Rewind shares the space above the input box.
        Open::Rewind(card) => over(
            frame,
            above,
            rewind::lines(
                &bingo_sdk::transcript::turns(&tree.viewed().transcript),
                card.selected,
                rows,
            ),
        ),
    }
}

/// One block, whole. What it shows is read from the item every frame, so a
/// block that is still arriving grows under the sheet rather than being copied
/// into it.
fn paged(
    tree: &Tree,
    frame: &mut Layout,
    above: Rect,
    open: &crate::pager::Pager,
    reveal: layers::Reveal,
) {
    let (title, content) = open.content(tree, above.width as usize);
    let window = pager::Window {
        height: content.len(),
        rows: usize::from(above.height).saturating_sub(pager::HEAD),
    };
    let lines = pager::sheet(&title, &content, open, window);
    sheet(frame, above, lines, reveal);
    marked(frame, above, open, window, reveal);
}

/// The hits of the pager's own query, once the sheet has finished arriving:
/// one still sliding up would carry the marks to the wrong rows.
fn marked(
    frame: &mut Layout,
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
fn sheet(frame: &mut Layout, above: Rect, lines: Vec<Line<'static>>, reveal: layers::Reveal) {
    layers::dim(frame);
    layers::sheet(frame, above, lines, reveal);
}

/// The open interaction, under the `⎿` of the row that asked: a bordered box,
/// or the form's band ([`dialog::shape`]). Its arrival is measured from when
/// the kernel opened it, so a card that was already up when this client
/// attached is simply there.
fn card(tree: &Tree, ui: &Ui, frame: &mut Layout, regions: Regions, now: Now) {
    let Some((owner, interaction)) = tree.open_interaction() else {
        return;
    };
    let asked_elsewhere = owner.summary.id != *tree.view();
    let agent = asked_elsewhere.then(|| tree::name(owner));
    let shape = dialog::shape(interaction);
    // Each row keeps the option it belongs to through the wrap, so a click
    // lands on what the eye is on. A box holds its rows in from each side; a
    // band draws them where the transcript's own rows are.
    let width = usize::from(regions.transcript.width.saturating_sub(2 * shape.inset()));
    let above = regions.above();
    // The shape's own rows and the title it keeps: what is left is what the
    // answers have to be walked in, and what a card lays itself out within.
    let room = usize::from(above.height).saturating_sub(usize::from(shape.chrome()) + 1);
    let mut drawing = crate::component::sealed::Site {
        state: owner,
        ui,
        interaction,
        agent: agent.as_deref(),
        width,
        room: room + 1,
    }
    .rows(&mut frame.pulls, opening(interaction, now).moving());
    let rows = wrapped_card_rows(std::mem::take(&mut drawing.rows), width);
    let rows = fitted_answers(rows, ui.dialog.focus, room);
    let lines: Vec<Line<'static>> = rows.iter().map(|(line, _)| line.clone()).collect();
    // Only a row of the transcript on screen can anchor it: a child's item
    // ids are its own, and would name the wrong row here.
    let anchor = (!asked_elsewhere)
        .then(|| asking_row(frame, interaction))
        .flatten();
    let at = layers::under(above, anchor, shape.height(lines.len(), above));
    drawing.place(Rect::new(
        at.x + shape.inset(),
        at.y + 1,
        width as u16,
        room as u16,
    ));
    frame.card_actions = drawing.actions;
    frame.card = Some(Card {
        area: at,
        options: rows.iter().map(|(_, option)| *option).collect(),
        sealed_options: drawing.options,
    });
    layers::dim(frame);
    layers::card(
        frame,
        at,
        shape,
        lines,
        opening(interaction, now),
        guarded(interaction, now),
        now,
    );
}

pub(crate) fn wrapped_card_rows(
    rows: Vec<(Line<'static>, Option<usize>)>,
    width: usize,
) -> Vec<(Line<'static>, Option<usize>)> {
    rows.into_iter()
        .flat_map(|(line, option)| {
            wrap::wrap_all(std::slice::from_ref(&line), width)
                .into_iter()
                .map(move |wrapped| (wrapped, option))
        })
        .collect()
}

/// The card's answers in the room the box has to walk them in: a question with
/// more options than that keeps the one the cursor is on, and says at which end
/// it cut the rest. What sits above them is [`layers::card`]'s to give away —
/// it keeps the title and the newest rows, which is what a permission wants:
/// the preview gives way, never the answers.
pub(crate) fn fitted_answers(
    rows: Vec<(Line<'static>, Option<usize>)>,
    focus: usize,
    room: usize,
) -> Vec<(Line<'static>, Option<usize>)> {
    let Some(first) = rows.iter().position(|(_, option)| option.is_some()) else {
        return rows;
    };
    let end = rows
        .iter()
        .rposition(|(_, option)| option.is_some())
        .map_or(first, |at| at + 1);
    // Whatever follows the answers — the mark of one already sent — is drawn
    // with them, so it is theirs to make room for.
    let room = room.saturating_sub(rows.len() - end);
    if end - first <= room {
        return rows;
    }
    let cursor = rows[first..end]
        .iter()
        .position(|(_, option)| *option == Some(focus))
        .unwrap_or(0);
    let at = window::of(end - first, cursor, room);
    let mut out: Vec<(Line<'static>, Option<usize>)> = rows[..first].to_vec();
    if at.above {
        out.push((window::cut(), None));
    }
    out.extend(
        rows[first + at.run.start..first + at.run.end]
            .iter()
            .cloned(),
    );
    if at.below {
        out.push((window::cut(), None));
    }
    out.extend(rows[end..].iter().cloned());
    out
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
fn asking_row(frame: &Layout, interaction: &bingo_sdk::Interaction) -> Option<u16> {
    let line = frame.blocks.span(interaction.item.as_ref()?)?.1;
    Some(frame.regions.transcript.y + frame.row_of(line)?)
}

/// How far into its arrival a card the kernel opened is. A still surface has
/// it whole from the first frame.
pub(crate) fn opening(interaction: &bingo_sdk::Interaction, now: Now) -> layers::Reveal {
    if !now.motion {
        return layers::Reveal::whole(layers::CARD_FRAMES);
    }
    let elapsed = now.past(interaction.opened_at);
    let since = now.instant.checked_sub(elapsed).unwrap_or(now.instant);
    layers::Reveal::at(layers::CARD_FRAMES, since, now.instant, false)
}

/// Rows that ride just above the input box, trimmed from the top when the
/// region cannot hold them all.
fn over(frame: &mut Layout, region: Rect, lines: Vec<Line<'static>>) {
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
/// it drew is left in [`crate::layout::Layout`] for the next key to read.
fn render_transcript(tree: &Tree, ui: &Ui, frame: &mut Layout, area: Rect, now: Now) {
    if area.height == 0 {
        return;
    }
    let rows = area.height as usize;
    let state = tree.viewed();
    let browse_folds = crate::browse::folds(ui, tree);
    let commands = ui.catalogs.specs();
    let rows_config = transcript::Rows::of(
        state,
        area.width as usize,
        &browse_folds,
        &commands,
        &frame.measurements,
        &ui.linked,
        now,
    )
    .in_tree(tree)
    .components(&ui.placed.pulled)
    .saying(ui.welcome.update.as_deref())
    .entrance(frame.canvas.buffer.area.height, ui.welcome.resume.as_ref())
    .measuring(ui.measure);
    frame.height = frame.blocks.sync(state, &tree.agents(), &rows_config);
    let inline = crate::ask::drawing::splice(tree, ui, frame, area, now);
    if let Some((_, removed, drawing)) = &inline {
        frame.height = frame.height - removed + drawing.rows.len();
    }
    frame.top = ui.scroll.top(frame.height, rows, now.instant);
    if let Some((at, _, drawing)) = &inline {
        frame.top = (at + drawing.rows.len()).saturating_sub(rows);
    }
    if let Some(top) = crate::browse::top(ui, tree, frame, rows.saturating_sub(1)) {
        frame.top = top;
    }
    component_requests(state, ui, frame, area);
    crate::component::closing::requests(state, ui, frame, area.width, area.height, now);
    let mut shown = if let Some((at, removed, drawing)) = &inline {
        let mut all = frame.blocks.window(0, frame.blocks.height());
        all.splice(
            *at..*at + removed,
            drawing.rows.iter().map(|(line, _)| line.clone()),
        );
        all.into_iter().skip(frame.top).take(rows).collect()
    } else {
        frame.blocks.window(frame.top, rows)
    };
    let padding = prepare_lines(ui, tree, frame, &mut shown);
    // Where the pictures among those lines landed, for a click on one (M56).
    frame.cells = graphics::placed::cells(&shown, area);
    frame.render_widget(Paragraph::new(shown), area);
    if let Some((at, _, drawing)) = inline {
        crate::ask::drawing::place(frame, drawing, at, area);
    }
    mark_transcript(ui, frame, area, padding, now);
}

fn prepare_lines(ui: &Ui, tree: &Tree, frame: &Layout, shown: &mut Vec<Line<'static>>) -> usize {
    crate::browse::mark(ui, tree, frame, shown);
    mark_row(ui, frame, shown);
    0
}

fn mark_transcript(ui: &Ui, frame: &mut Layout, area: Rect, padding: usize, now: Now) {
    // A mark is measured from the row line `top` was drawn at, which is where
    // the lines start rather than where the region does.
    let lined = lined(area, padding);
    if let Some(search) = ui.focus.search() {
        search::mark(&mut frame.canvas, lined, frame.top, search);
    }
    if let Some(run) = ui.focus.selection().as_ref() {
        select::mark(&mut frame.canvas, lined, frame.top, run);
    }
    if ui.crossfading(now) {
        layers::hush(frame, area);
    }
}

fn component_requests(state: &SessionState, ui: &Ui, frame: &mut Layout, area: Rect) {
    for item in state.transcript.items() {
        let Some((from, to)) = frame.blocks.span(&item.id) else {
            continue;
        };
        if to <= frame.top || from >= frame.top + usize::from(area.height) {
            continue;
        }
        for receipt in crate::ask::receipts::linked(state, &item.id) {
            if let Some(props) = crate::component::items::props(receipt, false)
                && let Some(key) = crate::component::key(
                    state,
                    &crate::component::items::id(receipt),
                    &props,
                    area.width,
                    area.height,
                )
            {
                frame.pulls.push(key);
            }
        }
        let expanded = crate::fold::fold_of(&ui.folds, item) == crate::fold::Fold::Open;
        for props in crate::component::items::all(item, expanded) {
            if let Some(key) = crate::component::key(
                state,
                &crate::component::items::id(item),
                &props,
                area.width,
                area.height,
            ) {
                let hides_children = matches!(
                    props,
                    bingo_sdk::component::Props::ToolGroup(_)
                        | bingo_sdk::component::Props::ToolUse(_)
                ) && ui
                    .placed
                    .pulled
                    .display(&key)
                    .is_some_and(|view| !crate::component::has_engine(view, props.component()));
                frame.pulls.push(key);
                if hides_children {
                    break;
                }
            }
        }
    }
}

/// Mark focus in the drawn copy, leaving the cached transcript unchanged.
fn mark_row(ui: &Ui, frame: &Layout, shown: &mut [Line<'static>]) {
    let Some(item) = ui.focus.row() else {
        return;
    };
    let Some((start, _)) = frame.blocks.span(item) else {
        return;
    };
    let Some(row) = start
        .checked_sub(frame.top)
        .and_then(|at| shown.get_mut(at))
    else {
        return;
    };
    if let Some(bullet) = row.spans.first_mut() {
        *bullet = theme::cursor_span(true);
    }
}

/// The rows of the region that carry a transcript line: a transcript shorter
/// than its region hangs from the composer, and the padding above it is
/// nobody's.
fn lined(area: Rect, padding: usize) -> Rect {
    let padding = u16::try_from(padding)
        .unwrap_or(area.height)
        .min(area.height);
    Rect {
        y: area.y + padding,
        height: area.height - padding,
        ..area
    }
}

/// The prompt box: the caret lives here and nowhere else. Its border is the
/// one box the frame draws itself; what is inside it is [`crate::composer`]'s.
fn render_composer(
    state: &SessionState,
    ui: &Ui,
    frame: &mut Layout,
    area: Rect,
    now: Now,
    draft: prompt::Layout,
) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            theme::rule().repeat(usize::from(area.width)),
            theme::rule_style(),
        ))),
        Rect { height: 1, ..area },
    );
    let inner = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    if let Some(search) = ui.focus.search() {
        frame.render_widget(Paragraph::new(search::row(search)), inner);
        return;
    }
    if inner.height > 0 {
        render_draft(state, ui, frame, inner, draft);
        if let Some(cell) = frame.buffer_mut().cell_mut((area.x + 1, inner.y)) {
            cell.set_style(theme::text());
            cell.set_style(if state.busy() {
                activity::breathing(state, now)
            } else {
                theme::glow()
            });
        }
    }
}

/// The thumbnails, standing on the box's top border in the rows the frame
/// cut for them, indented to the prompt's own column so a picture sits over
/// the words it belongs to. A frame that cut no rows for it — the screen was
/// too short — draws none of it: what is being typed matters more than what
/// was pasted beside it.
fn render_strip(frame: &mut Layout, area: Rect, strip: graphics::Band) {
    let rows = strip.height();
    if rows == 0 || area.height < rows {
        frame.strip.clear();
        return;
    }
    let indent = 2;
    let band = Rect {
        x: area.x + indent,
        width: area.width.saturating_sub(indent),
        ..area
    };
    frame
        .cells
        .extend(graphics::placed::cells(&strip.lines, band));
    frame.strip = strip.pictures;
    frame.render_widget(Paragraph::new(strip.lines), band);
}

/// The draft itself and the caret in it. `width` is the box's, borders and
/// all, which is what says how wide a row of text may be.
fn render_draft(
    state: &SessionState,
    ui: &Ui,
    frame: &mut Layout,
    area: Rect,
    layout: prompt::Layout,
) {
    let props = crate::keybar::composer(state, ui, frame);
    let prompt = format!("{}{}", prompt::prompt(state), props.tag);
    let start = layout.cursor.0.saturating_sub(COMPOSER_ROWS - 1);
    let placeholder = props.placeholder;
    let lines = prompt::box_lines(
        &layout,
        &prompt,
        (start, COMPOSER_ROWS),
        ui.composer.is_empty().then_some(placeholder.as_str()),
    );
    frame.render_widget(Paragraph::new(lines), area);
    frame.set_cursor_position((
        area.x
            + u16::try_from(layout.cursor.1 + prompt.width())
                .unwrap_or(u16::MAX)
                .min(area.width.saturating_sub(1)),
        area.y
            + u16::try_from(layout.cursor.0 - start)
                .unwrap_or(u16::MAX)
                .min(area.height.saturating_sub(1)),
    ));
}

/// The `/` and `@` dropdown: what the line being typed ranks, in `rows` of
/// room around the one the cursor is on.
fn menu(ui: &Ui, cwd: &str, mentions: &[String], rows: usize) -> Vec<Line<'static>> {
    let suggestions = ui.suggestions(cwd, mentions);
    let selected = ui.menu.selected.min(suggestions.len().saturating_sub(1));
    let listed = listed(&suggestions, selected);
    // Asked of the lines themselves rather than counted from the labels: a
    // second sum of where a label falls is a second place to get it wrong
    // ([`crate::roster`]).
    let at = listed
        .iter()
        .position(|(_, of)| *of == Some(selected))
        .unwrap_or(0);
    let lines = listed.into_iter().map(|(line, _)| line).collect();
    window::around(lines, at, rows)
}

/// Every line the dropdown has before the window takes it: its rows, each under
/// the label of the run it is in where there is another run beside it, and the
/// row of the list each line answers to. A label answers none — it is furniture,
/// like the roster's, and nowhere the cursor lands.
fn listed(suggestions: &[Suggestion], selected: usize) -> Vec<(Line<'static>, Option<usize>)> {
    let labelled = suggestions
        .windows(2)
        .any(|two| two[0].group != two[1].group);
    let column = suggestions
        .iter()
        .map(|row| row.label.width())
        .max()
        .unwrap_or(0);
    let mut out: Vec<(Line<'static>, Option<usize>)> = Vec::new();
    let mut run: Option<Group> = None;
    for (index, row) in suggestions.iter().enumerate() {
        if labelled && run != Some(row.group) {
            out.push((run_label(row.group), None));
        }
        run = Some(row.group);
        out.push((suggestion_line(row, column, index == selected), Some(index)));
    }
    out
}

/// What a run is called: dim, at the margin its rows are indented from.
fn run_label(group: Group) -> Line<'static> {
    Line::from(Span::styled(group.label().to_string(), theme::dim()))
}

/// One row: the mark where the cursor is, the label padded to the widest of
/// them, and whatever the row has to say after it.
fn suggestion_line(row: &Suggestion, column: usize, focused: bool) -> Line<'static> {
    let style = if focused { theme::text() } else { theme::dim() };
    let label = format!("{:<column$}", row.label, column = column);
    Line::from(vec![
        theme::cursor_span(focused),
        Span::styled(
            format!("{label}  {}", row.hint).trim_end().to_string(),
            style,
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;
    use crate::test_support::*;
    use bingo_sdk::{
        ContentPart, ContextUsage, Event, InterruptReason, ItemBody, ItemStatus, KernelError,
        Level, Preview, QueueEntry, ToolOutput, TurnId, TurnStatus, View,
    };
    use crossterm::event::KeyCode;
    use serde_json::json;

    fn item_frame(seq: u64, item: bingo_sdk::Item) -> bingo_sdk::Frame {
        frame(seq, Event::ItemCompleted { item })
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
        let (mut ui, now) = mid_turn();
        insta::assert_snapshot!(render(&state, &mut ui, now));
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
        let (mut ui, now) = mid_turn();
        insta::assert_snapshot!(render(&state, &mut ui, now));
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
        let (mut ui, now) = scene();
        insta::assert_snapshot!(render(&state, &mut ui, now));
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
        let (mut ui, now) = scene();
        insta::assert_snapshot!(render(&state, &mut ui, now));
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
        insta::assert_snapshot!(draw_sized(80, 34, &state, &mut ui, now));
    }

    #[test]
    fn permission_with_the_feedback_row_open() {
        let state = folded(vec![frame(
            1,
            opened(permission(
                None,
                Some(Preview::Command {
                    command: "rm -rf build".into(),
                    description: None,
                    cwd: "/tmp/project".into(),
                }),
            )),
        )]);
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        crate::input::on_key(&mut ui, &solo(&state), typed('n'), now);
        write(&mut ui, &state, "use cargo clean", now);
        insta::assert_snapshot!(render(&state, &mut ui, now));
    }

    #[test]
    fn a_card_comes_down_from_its_top_edge() {
        let asked = folded(vec![frame(1, opened(permission(Some("Edit(src/)"), None)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(asked.interactions.first());
        // Frame 0 is the screen before the kernel opened it; then one frame
        // every 33 ms until the card is whole.
        let mut screens = vec![render(&state(), &mut ui, now)];
        screens.extend((0..3).map(|f| render(&asked, &mut ui, later(now, f * 33))));
        assert_eq!(
            screens[3],
            render(&asked, &mut ui, later(now, 500)),
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
        // Few enough that the asking row stays on screen above the reserved
        // activity band (view.rs's demand keeps two rows for it).
        frames.extend((3..10).map(|i| {
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
        let screen = render(&state, &mut ui, now);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows.iter().any(|row| row.contains("needs you")), "{screen}");
        assert!(
            !rows.iter().any(|row| row.contains("Edit(src/lib.rs)")),
            "the waiting step is replaced by its sealed ask: {screen}"
        );
        assert!(
            rows.last().is_some_and(|row| row.contains("fake-1")),
            "and the frame under it did not move:\n{screen}"
        );
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn an_inline_ask_does_not_dim_the_conversation() {
        let state = folded(vec![
            item_frame(1, user("itm_1", "edit it")),
            frame(2, opened(permission(None, None))),
        ]);
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        let screen = drawn(80, 24, &solo(&state), &mut ui, now);
        assert!(screen.buffer().content().iter().any(|cell| {
            cell.symbol() == "e"
                && !cell
                    .style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::DIM)
        }));
        assert!(render(&state, &mut ui, now).contains("needs you"));
    }

    #[test]
    fn dropdown() {
        let state = state();
        let (mut ui, now) = scene();
        ui.catalogs.commands = crate::commands::entries(vec![bingo_sdk::CommandSpec {
            opens: false,
            keys: vec![],
            hidden: false,
            name: "model".into(),
            aliases: vec![],
            hint: "[provider/]model".into(),
            args: bingo_sdk::ArgSpec::Catalog {
                source: "models".into(),
            },
            instant: true,
            family: "kernel".into(),
        }]);
        write(&mut ui, &state, "/", now);
        insta::assert_snapshot!(render(&state, &mut ui, now));
    }

    #[test]
    fn the_first_ctrl_c_says_how_to_leave() {
        let state = state();
        let (mut ui, now) = scene();
        crate::input::on_key(&mut ui, &solo(&state), ctrl('c'), now);
        insta::assert_snapshot!(render(&state, &mut ui, now));
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
        let (mut ui, now) = mid_turn();
        insta::assert_snapshot!(render(&state, &mut ui, now));
    }

    /// A queue as the kernel published it, each entry labelled by the surface
    /// that put it there and by whether the running turn may be steered with
    /// it (ADR-0008 §2, amended M68).
    fn queue_frame(seq: u64, entries: &[(&str, &str, bool)]) -> bingo_sdk::Frame {
        frame(
            seq,
            Event::QueueChanged {
                revision: seq,
                entries: entries
                    .iter()
                    .enumerate()
                    .map(|(i, (surface, preview, steerable))| QueueEntry {
                        intent: bingo_sdk::IntentId::from_raw(format!("req_{i}")),
                        position: i as u32,
                        preview: (*preview).to_string(),
                        steerable: *steerable,
                        origin: bingo_sdk::Origin::surface(*surface),
                    })
                    .collect(),
            },
        )
    }

    /// The band under the transcript, drawn from a queue of `(surface,
    /// preview)`. The turn has only just started, so nothing but the queue asks
    /// for a row: what the band holds is what the queue put there.
    fn band(entries: &[(&str, &str, bool)]) -> String {
        let (mut ui, now) = scene();
        render(
            &folded(vec![frame(1, started("trn_1")), queue_frame(2, entries)]),
            &mut ui,
            now,
        )
    }

    /// ADR-0028: the pending area is the person's own queue and nothing else.
    /// A subsystem's entry is a steer in flight — it is drawn nowhere, and the
    /// transcript shows it as a quiet notice once the turn absorbs it — and so
    /// is an unstamped entry from any surface a person does not type through.
    #[test]
    fn the_pending_area_draws_only_what_the_person_queued() {
        let mine = band(&[("tui", "also fix the docs", true)]);
        assert!(mine.contains("› also fix the docs"), "{mine}");

        let steer = band(&[("room", "the build is green", true)]);
        assert_eq!(
            steer,
            band(&[]),
            "a steer in flight draws nothing at all, not even the blank row it \
             would have been spaced from the transcript by"
        );

        let both = band(&[
            ("room", "the build is green", true),
            ("tui", "also fix the docs", true),
        ]);
        assert!(both.contains("› also fix the docs"), "{both}");
        assert!(!both.contains("the build is green"), "{both}");

        assert_eq!(
            band(&[("brand-new", "who knows", true)]),
            band(&[]),
            "a surface nobody has judged is not the person's"
        );
    }

    /// The two keys that send do different things while a turn runs, so the
    /// rows say which line is which and the box says which key is which
    /// (M68). `steerable` is the one reading of it: a queued command has
    /// always waited for the turn to end, and now says so too.
    #[test]
    fn a_line_that_will_not_steer_wears_the_tag_and_the_busy_box_says_why() {
        let steers = band(&[("tui", "also fix the docs", true)]);
        assert!(steers.contains("› also fix the docs"), "{steers}");
        assert!(!steers.contains("(waits)"), "{steers}");

        let waits = band(&[("tui", "and then the docs", false)]);
        assert!(waits.contains("› and then the docs (waits)"), "{waits}");

        assert!(waits.contains("⏎ queue a message"), "{waits}");
        assert!(!waits.contains(keys::PLACEHOLDER), "{waits}");
        let idle = render(&state(), &mut scene().0, scene().1);
        assert!(idle.contains(keys::PLACEHOLDER), "{idle}");
        assert!(idle.contains(" ▍ ask anything"), "{idle}");
    }

    /// The rows the band asks for, which the screen above cannot show: a blank
    /// spacer among a region of blanks looks like nothing either way. A steer
    /// in flight costs not even that row — the band is empty, so the frame
    /// gives it nothing — while a person's entry brings the row and the air
    /// above it (§3).
    #[test]
    fn a_steer_in_flight_asks_the_frame_for_no_row_at_all() {
        let (ui, now) = scene();
        let rows = |entries: &[(&str, &str, bool)]| {
            activity::lines(
                &solo(&folded(vec![
                    frame(1, started("trn_1")),
                    queue_frame(2, entries),
                ])),
                &ui,
                80,
                now,
            )
            .len()
        };
        assert_eq!(rows(&[("room", "the build is green", true)]), 0);
        assert_eq!(
            rows(&[("tui", "also fix the docs", true)]),
            2,
            "the row and its air"
        );
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
        let (mut ui, now) = scene();
        insta::assert_snapshot!(render(&state, &mut ui, now));
    }

    fn with_context(used: u64) -> bingo_sdk::SessionState {
        folded(vec![frame(
            1,
            Event::TurnUsage {
                turn: TurnId::from_raw("trn_1"),
                usage: Default::default(),
                context: ContextUsage {
                    parts: None,
                    used,
                    window: 200_000,
                    trigger: 180_000,
                },
            },
        )])
    }

    #[test]
    fn the_context_notice_sits_on_the_status_line_when_it_is_true() {
        let (mut ui, now) = scene();
        insta::assert_snapshot!(render(&with_context(170_000), &mut ui, now));
    }

    #[test]
    fn the_status_line_names_the_mode_the_policy_published() {
        let (mut ui, now) = scene();
        insta::assert_snapshot!(render(&with_permission_mode("acceptEdits"), &mut ui, now));
    }

    #[test]
    fn a_config_without_a_mode_leaves_the_status_line_as_it_was() {
        let published = folded(vec![frame(1, plugin_view("hooks", json!({"events": 3})))]);
        let (mut ui, now) = scene();
        assert_eq!(
            render(&published, &mut ui, now),
            render(&state(), &mut ui, now),
            "no badge until a policy publishes one"
        );
    }

    #[test]
    fn a_rejected_intent_becomes_a_notice() {
        let state = state();
        let (mut ui, now) = scene();
        ui.notify(Level::Error, "unknown command: /x", now.instant);
        insta::assert_snapshot!(render(&state, &mut ui, now));
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
        insta::assert_snapshot!(render(&state, &mut ui, now));
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
        let (mut ui, now) = scene();
        let screen = render_tree(&tree, &mut ui, now);
        assert!(screen.contains("◗ reviewer(review the diff)"), "{screen}");
        assert!(screen.contains("⎿  working"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn a_child_that_needs_a_person_is_counted_on_the_status_line() {
        let tree = spawned(vec![child_frame(2, opened(child_permission()))]);
        let (mut ui, now) = settled();
        ui.dialog
            .focus_on(tree.open_interaction().map(|(_, open)| open));
        let screen = render_tree(&tree, &mut ui, now);
        assert!(screen.contains("◗"), "{screen}");
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
        let (mut ui, now) = mid_turn();
        let screen = render_tree(&tree, &mut ui, now);
        assert!(screen.contains("reviewer · session"), "{screen}");
        assert!(screen.contains("Two nits"), "{screen}");
        insta::assert_snapshot!(screen);
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
            posted(
                2,
                "itm_1",
                "reviewer",
                "the plan is thin on tests\nnone of M9's exit criteria name one",
            ),
            posted(3, "itm_2", "scout", "M9's exit criteria cover them"),
            log_frame(
                4,
                Event::ItemCompleted {
                    item: user("itm_3", "then let us ship it"),
                },
            ),
        ]);
        let (mut ui, now) = scene();
        let screen = render_tree(&tree, &mut ui, now);
        assert!(screen.contains("◗ reviewer: the plan"), "{screen}");
        assert!(
            screen.contains("  none of M9's exit criteria name one"),
            "the rest of a message is prose under the name, not a folded \
             result: {screen}"
        );
        assert!(!screen.contains("⎿"), "{screen}");
        assert!(screen.contains("✦ scout: M9's"), "{screen}");
        assert!(
            screen.contains("✦ parent: then let us ship it"),
            "and what the person typed is a post like any other, under the \
             name the roster reads it by: {screen}"
        );
        assert!(
            !screen.contains("running") && !screen.contains("idle"),
            "nothing answers a room: {screen}"
        );
    }

    #[test]
    fn the_composer_of_a_room_offers_to_post_to_it() {
        let (mut ui, now) = scene();
        let screen = render_tree(&room(vec![]), &mut ui, now);
        assert!(screen.contains("#design › post to the room"), "{screen}");
        insta::assert_snapshot!(screen);
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

    /// A room's board is the room's own list, drawn where any list is: under
    /// the activity row of the room's own view, and nowhere in a member's.
    #[test]
    fn a_room_s_board_is_drawn_in_the_room_s_view_and_not_in_its_parent_s() {
        let mut tree = room(vec![log_frame(2, tasks())]);
        let (mut ui, now) = scene();
        let board = render_tree(&tree, &mut ui, now);
        assert!(
            board.contains("2 tasks (0 done, 1 in progress, 1 open)"),
            "{board}"
        );
        assert!(board.contains("◼ ship it — reviewer"), "{board}");

        let root = tree.root_id().clone();
        tree.show(&root);
        assert!(!render_tree(&tree, &mut ui, now).contains("ship it"));
    }

    // ---- the three lanes (ADR-0013) -------------------------------------

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
                "Board",
                json!({}),
                Some(output),
                ItemStatus::Completed,
            ),
        )]);
        let (mut ui, now) = scene();
        let screen = render(&state, &mut ui, now);
        assert!(screen.contains("⎿  id  task"), "{screen}");
        assert!(!screen.contains("2 open"), "the model's text is not drawn");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn a_full_draw_of_a_long_transcript_reuses_the_cached_blocks() {
        let state = long_transcript(200);
        let (mut ui, now) = scene();
        let first = draw_sized(120, 40, &state, &mut ui, now);
        assert_eq!(ui.shown.blocks.renders(), 200);
        for _ in 0..2 {
            assert_eq!(draw_sized(120, 40, &state, &mut ui, now), first);
        }
        assert_eq!(
            ui.shown.blocks.renders(),
            200,
            "warm draws reuse every finished block"
        );
    }

    #[test]
    fn a_terminal_too_small_for_anything_still_draws() {
        let (mut ui, now) = scene();
        for (width, height) in [(1u16, 1u16), (4, 2), (10, 3), (20, 5)] {
            draw_sized(width, height, &state(), &mut ui, now);
        }
    }

    #[test]
    fn ctrl_f_searches_the_transcript_and_esc_gives_the_status_line_back() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
        let press = |ui: &mut Ui, key| {
            crate::input::on_key(ui, &solo(&state), key, now);
        };
        press(&mut ui, ctrl('f'));
        for c in "line 4".chars() {
            press(&mut ui, typed(c));
        }
        let typing = render(&state, &mut ui, now);
        assert!(typing.contains("/line 4▌"), "{typing}");

        press(&mut ui, key(crossterm::event::KeyCode::Enter));
        // The transcript eases to the hit; this is where it lands.
        let there = Now {
            instant: now.instant + crate::scroll::EASE,
            ..now
        };
        let found = render(&state, &mut ui, there);
        assert!(found.contains("1/11"), "{found}");
        assert!(found.contains("line 4"), "it scrolled to the hit: {found}");
        insta::assert_snapshot!(found);

        press(&mut ui, typed('n'));
        let stepped = render(&state, &mut ui, there);
        assert!(stepped.contains("2/11"), "{stepped}");

        press(&mut ui, key(crossterm::event::KeyCode::Esc));
        assert!(
            render(&state, &mut ui, there).contains("⏵⏵ default"),
            "esc gives the status line back"
        );
    }

    #[test]
    fn a_hit_on_the_screen_is_marked_where_it_sits() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
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
            &mut ui,
            Now {
                instant: now.instant + crate::scroll::EASE,
                ..now
            },
        );
        let marked = hit_cells(&screen);
        assert_eq!(marked.len(), 7, "seven cells of `line 59`: {marked:?}");
        assert!(
            marked.iter().all(|(x, _)| (3..10).contains(x)),
            "after the `❯ `: {marked:?}"
        );
    }

    /// The cells carrying the mark a hit is drawn in.
    fn hit_cells(screen: &ratatui::backend::TestBackend) -> Vec<(u16, u16)> {
        screen
            .buffer()
            .content()
            .iter()
            .enumerate()
            .filter(|(i, cell)| {
                *i >= 160 && cell.style() == theme::as_drawn(theme::presence().patch(theme::bold()))
            })
            .map(|(i, _)| ((i % 80) as u16, (i / 80) as u16))
            .collect()
    }

    /// A hit is marked the way a run is tinted: on a transcript shorter than
    /// its region, both hang with the lines and not with the pane.
    #[test]
    fn a_hit_on_a_short_transcript_is_marked_where_it_sits() {
        let state = folded(vec![item_frame(1, user("itm_1", "run the tests"))]);
        let tree = solo(&state);
        let (mut ui, now) = scene();
        let row = row_carrying(&render(&state, &mut ui, now), "run the tests");
        crate::input::on_key(&mut ui, &tree, ctrl('f'), now);
        for c in "tests".chars() {
            crate::input::on_key(&mut ui, &tree, typed(c), now);
        }
        crate::input::on_key(&mut ui, &tree, key(crossterm::event::KeyCode::Enter), now);
        assert_eq!(
            hit_cells(&drawn(80, 24, &tree, &mut ui, now)),
            (11..16).map(|x| (x, row)).collect::<Vec<_>>(),
            "the five cells of `tests` on the row it is drawn on"
        );
    }

    /// The cells one draw styles differently from another, gathered by row:
    /// where a mark landed, and nowhere else.
    fn marked_cells(
        quiet: &ratatui::backend::TestBackend,
        marked: &ratatui::backend::TestBackend,
    ) -> Vec<(u16, String)> {
        let width = usize::from(marked.buffer().area().width);
        let mut rows: Vec<(u16, String)> = Vec::new();
        for (i, (_, after)) in quiet
            .buffer()
            .content()
            .iter()
            .zip(marked.buffer().content())
            .enumerate()
            .filter(|(_, (before, after))| before.style() != after.style())
        {
            let row = u16::try_from(i / width).expect("a row of the screen");
            match rows.last_mut() {
                Some((at, text)) if *at == row => text.push_str(after.symbol()),
                _ => rows.push((row, after.symbol().to_string())),
            }
        }
        rows
    }

    /// A drag across the row a needle is on: the row it went over, and the
    /// cells the tint landed on — the round trip a person sees.
    fn dragged_over(
        state: &SessionState,
        needle: &str,
        from: u16,
        to: u16,
    ) -> (u16, Vec<(u16, String)>) {
        let tree = solo(state);
        let (mut ui, now) = scene();
        let row = row_carrying(&render(state, &mut ui, now), needle);
        crate::pointer::on_mouse(&mut ui, &tree, click(from, row), now);
        crate::pointer::on_mouse(&mut ui, &tree, dragged(to, row), now);
        let marked = drawn(80, 24, &tree, &mut ui, now);
        ui.clear_selection();
        (
            row,
            marked_cells(&drawn(80, 24, &tree, &mut ui, now), &marked),
        )
    }

    /// A transcript shorter than its region hangs from the composer, so the
    /// rows above it carry no line at all. The tint hangs with it: what a drag
    /// marks is the text under the pointer, not the row that far from the top
    /// of the pane.
    #[test]
    fn a_drag_over_a_short_transcript_tints_the_cells_under_the_pointer() {
        let state = folded(vec![item_frame(1, user("itm_1", "run the tests"))]);
        let (row, marked) = dragged_over(&state, "run the tests", 3, 6);
        assert_eq!(marked, vec![(row, "run".to_string())]);
    }

    /// The same drag on a transcript taller than its region, where there is no
    /// padding to get wrong: one mapping answers both.
    #[test]
    fn a_drag_over_a_long_transcript_tints_the_cells_under_the_pointer() {
        let (row, marked) = dragged_over(&long_transcript(60), "line 59", 3, 7);
        assert_eq!(marked, vec![(row, "line".to_string())]);
    }

    /// A wide glyph is drawn in two cells, and the tint covers both: a run is
    /// measured the way the terminal measures, not in characters.
    #[test]
    fn a_drag_over_wide_glyphs_tints_the_cells_they_are_drawn_in() {
        let state = folded(vec![item_frame(1, user("itm_1", "你好 warm"))]);
        let (row, marked) = dragged_over(&state, "你好", 3, 7);
        assert_eq!(marked, vec![(row, "你好".to_string())]);
    }

    #[test]
    fn the_transcript_scrolls_back_a_page() {
        let state = folded(
            (1..=30)
                .map(|i| item_frame(i, user(&format!("itm_{i}"), &format!("line {i}"))))
                .collect(),
        );
        let (mut ui, now) = scene();
        let bottom = render(&state, &mut ui, now);
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
        let scrolled = render(&state, &mut ui, settled);
        assert_ne!(bottom, scrolled, "page up must move the window");
        insta::assert_snapshot!(scrolled);
    }

    /// The pill takes the band's air, which the frame holds anyway (§3): the
    /// transcript is the same rows held as following, so a scroll never
    /// moves the frame (M95).
    #[test]
    fn the_foot_row_costs_the_transcript_nothing() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
        let following = ui.shown.regions;
        crate::input::on_key(
            &mut ui,
            &solo(&state),
            key(crossterm::event::KeyCode::PageUp),
            now,
        );
        let settled = later(now, 100);
        let screen = render(&state, &mut ui, settled);
        assert!(screen.contains("lines below"), "{screen}");
        assert_eq!(ui.shown.regions, following);
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
        assert!(render(&state, &mut ui, now).contains("+4 lines (ctrl+o to expand)"));
        let tree = solo(&state);
        crate::input::on_key(&mut ui, &tree, ctrl('o'), now);
        let opened = render(&state, &mut ui, now);
        assert!(opened.contains("line 9"), "{opened}");
        assert!(!opened.contains("+4 lines"), "{opened}");

        crate::input::on_key(&mut ui, &tree, ctrl('o'), now);
        let paged = render(&state, &mut ui, later(now, 200));
        assert!(
            paged.contains("Read(src/lib.rs)"),
            "the sheet says what it is: {paged}"
        );
        assert!(paged.contains("j/k"), "{paged}");

        crate::input::on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(
            render(&state, &mut ui, later(now, 200)).contains("+4 lines (ctrl+o to expand)"),
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
        let before = render(&state, &mut ui, now);

        // A click focuses the block; `⏎` opens it whole.
        crate::input::on_key(&mut ui, &tree, ctrl('o'), now);
        crate::input::on_key(&mut ui, &tree, ctrl('o'), now);
        let settled = later(now, 200);
        let top = render(&state, &mut ui, settled);
        assert!(top.contains("line 1"), "{top}");

        crate::input::on_key(&mut ui, &tree, typed('G'), now);
        let bottom = render(&state, &mut ui, settled);
        assert!(bottom.contains("line 40"), "{bottom}");
        assert_ne!(top, bottom, "G takes the end of it");

        crate::input::on_key(&mut ui, &tree, typed('/'), now);
        for c in "line 7".chars() {
            crate::input::on_key(&mut ui, &tree, typed(c), now);
        }
        let typing = render(&state, &mut ui, settled);
        assert!(typing.contains("/line 7"), "{typing}");
        crate::input::on_key(&mut ui, &tree, key(KeyCode::Enter), now);
        let found = render(&state, &mut ui, settled);
        assert!(found.contains("1/1"), "{found}");
        assert!(found.contains("line 7"), "{found}");

        crate::input::on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        crate::input::on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert_eq!(
            render(&state, &mut ui, later(now, 400)),
            before,
            "the frame beneath is what it was"
        );
    }

    // ---- M48: the strip of thumbnails standing on the box -----------

    /// A draft carrying pictures, as a person leaves it: the tokens in the
    /// line and the pictures held behind them.
    fn carrying(ui: &mut Ui, pictures: usize) {
        for _ in 0..pictures {
            crate::test_support::drafted(ui, bingo_pictures::testing::png(100, 200));
        }
    }

    /// The rows of the input box, from its top border down: the last box on
    /// the screen, the welcome box being above it.
    fn box_rows(screen: &str) -> Vec<String> {
        let lines: Vec<&str> = screen.lines().collect();
        let top = lines
            .iter()
            .rposition(|line| line.trim_matches('"').starts_with("──"))
            .unwrap_or_else(|| panic!("no prompt rule:\n{screen}"));
        lines[top..]
            .iter()
            .take(2)
            .map(|line| (*line).to_string())
            .collect()
    }

    fn placeholder_rows(screen: &str) -> usize {
        screen
            .lines()
            .filter(|line| line.contains(crate::graphics::kitty::PLACEHOLDER))
            .count()
    }

    /// A pasted picture is three rows of cells standing on the box's top
    /// border, and the box itself is exactly the height it was.
    #[test]
    fn a_carried_picture_puts_a_strip_on_the_box() {
        let state = state();
        let (mut ui, now) = scene();
        let plain = render(&state, &mut ui, now);
        crate::graphics::with(crate::graphics::drawing(), || {
            carrying(&mut ui, 1);
            let screen = render(&state, &mut ui, now);
            assert_eq!(box_rows(&screen).len(), box_rows(&plain).len(), "{screen}");
            assert_eq!(
                placeholder_rows(&screen),
                usize::from(graphics::band::ROWS),
                "{screen}"
            );
            let lines: Vec<&str> = screen.lines().collect();
            let top = lines
                .iter()
                .rposition(|line| line.trim_matches('"').starts_with("──"))
                .expect("the prompt rule");
            assert!(
                lines[top - usize::from(graphics::band::ROWS)..top]
                    .iter()
                    .all(|row| row.contains(crate::graphics::kitty::PLACEHOLDER)),
                "the cells are the rows right above the border: {screen}"
            );
            let first = lines[top - usize::from(graphics::band::ROWS)].trim_start_matches('"');
            assert!(
                first.starts_with("  ") && first.chars().nth(2) != Some(' '),
                "and stand in the prompt's own column: {screen}"
            );
            assert!(
                box_rows(&screen)[1].contains("[image 1]"),
                "the token is still in the words, on the box's first row: {screen}"
            );
        });
    }

    /// The line is the record: deleting the token takes the strip with it and
    /// gives the transcript its rows back.
    #[test]
    fn deleting_the_token_takes_the_strip_with_it() {
        let state = state();
        let (mut ui, now) = scene();
        crate::graphics::with(crate::graphics::drawing(), || {
            let plain = render(&state, &mut ui, now);
            carrying(&mut ui, 1);
            assert_eq!(placeholder_rows(&render(&state, &mut ui, now)), 3);
            ui.composer.clear();
            let after = render(&state, &mut ui, now);
            assert_eq!(placeholder_rows(&after), 0, "{after}");
            assert_eq!(after, plain, "and the box is what it was");
        });
    }

    /// Four thumbnails and a count of what would not fit.
    #[test]
    fn five_carried_pictures_show_four_and_a_count() {
        let state = state();
        let (mut ui, now) = scene();
        crate::graphics::with(crate::graphics::drawing(), || {
            carrying(&mut ui, 5);
            let screen = render(&state, &mut ui, now);
            assert!(screen.contains("+1"), "{screen}");
            assert_eq!(ui.shown.strip.len(), graphics::band::SHOWN, "{screen}");
        });
    }

    /// A terminal that draws no pictures gets no band: the tokens in the line
    /// already say what is attached.
    #[test]
    fn a_terminal_that_draws_no_pictures_gets_no_strip() {
        let state = state();
        let (mut ui, now) = scene();
        carrying(&mut ui, 2);
        let screen = render(&state, &mut ui, now);
        assert_eq!(placeholder_rows(&screen), 0, "{screen}");
        assert!(screen.contains("[image 1]"), "{screen}");
        assert!(ui.shown.strip.is_empty());
    }

    /// A room is posted into rather than asked, and its prompt keeps the
    /// box's first row with the strip standing above the border.
    #[test]
    fn a_rooms_prompt_keeps_its_row_under_the_strip() {
        let tree = room_tree(Vec::new());
        let (mut ui, now) = scene();
        crate::graphics::with(crate::graphics::drawing(), || {
            carrying(&mut ui, 1);
            let screen = render_tree(&tree, &mut ui, now);
            let rows = box_rows(&screen);
            let prompt = rows
                .iter()
                .position(|row| row.contains("#design ›"))
                .unwrap_or_else(|| panic!("the room's own prompt: {screen}"));
            assert_eq!(prompt, 1, "{rows:?}");
            assert_eq!(placeholder_rows(&screen), usize::from(graphics::band::ROWS));
        });
    }

    /// The widest drawn row carrying the answer's own words. The welcome box
    /// and the input box are the width of the transcript whatever prose does,
    /// so they are not what is being measured here.
    fn widest_prose(screen: &ratatui::backend::TestBackend) -> usize {
        let width = usize::from(screen.buffer().area().width);
        screen
            .buffer()
            .content()
            .chunks(width)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .filter(|row| row.contains("word"))
            .map(|row| row.trim_end().width())
            .max()
            .unwrap_or(0)
    }

    /// A wide terminal is filled with prose, and `tui.measure` is the one
    /// thing that narrows it (design §7, M89). Drawn rather than counted: the
    /// measure travels from the run's own arguments through [`Ui`] to the
    /// rows, and this is the whole of that seam.
    #[test]
    fn a_wide_terminal_is_filled_unless_a_person_set_a_measure() {
        let state = folded(vec![item_frame(
            1,
            assistant("itm_1", &"word ".repeat(80), ItemStatus::Completed),
        )]);
        let tree = solo(&state);
        let (mut ui, now) = scene();
        let filled = widest_prose(&drawn(200, 40, &tree, &mut ui, now));
        assert!(filled > 150, "prose fills the terminal: {filled} cells");
        assert!(filled <= 200, "and never overruns it: {filled} cells");
        ui.measure = Some(100);
        let capped = widest_prose(&drawn(200, 40, &tree, &mut ui, now));
        assert!(capped <= 100, "a person who set one has it: {capped} cells");
        assert!(capped > 80, "and it is the measure, not a guess: {capped}");
    }

    /// The frame yields the strip before it yields the row a person is
    /// typing on: a screen too short for both keeps the prompt.
    #[test]
    fn a_screen_too_short_for_both_keeps_the_prompt_and_not_the_strip() {
        let state = state();
        let (mut ui, now) = scene();
        crate::graphics::with(crate::graphics::drawing(), || {
            carrying(&mut ui, 1);
            let screen = draw_sized(80, 5, &state, &mut ui, now);
            assert_eq!(placeholder_rows(&screen), 0, "{screen}");
            assert!(screen.contains("[image 1]"), "{screen}");
        });
    }
}
