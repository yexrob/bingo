//! The stream, dock and footer in one frame.
//! `compose` fills an owned layout without borrowing a terminal frame.

mod welcome;
use crate::layout::Layout;
use bingo_sdk::SessionState;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::clock::Now;
use crate::frame::{self, Demand, Regions};
use crate::tree::{self, Tree};
use crate::ui::{Card, Open, Ui};
use crate::{
    activity, composer as prompt, dialog, graphics, layers, pill, search, select, theme,
    transcript, views, window, wrap,
};

/// How tall the composer box may grow before it scrolls internally.
const COMPOSER_ROWS: usize = 8;

/// One render path for the whole tree: it paints the session in view and
/// derives everything about the others — the counts in the footer, the
/// `↳` rows, the switcher — from their states.
pub fn compose(tree: &Tree, ui: &Ui, frame: &mut Layout, now: Now) {
    let tools = crate::tool_rows::Looks::in_ui(ui);
    let area = frame.area();
    let state = tree.viewed();
    let draft = Draft {
        layout: ui
            .composer
            .layout(prompt::width(area.width as usize))
            .images(&ui.pictures, ui.composer.text()),
        above: composer_above(tree, ui, frame, now),
    };
    let activity = activity::components(tree, ui, area.width as usize, now);
    let activity_next = activity.next;
    let mut activity = activity.lines;
    let band_offset = activity.len();
    let band = crate::component::band::draw(state, ui, frame, now);
    activity.extend(band.lines.iter().cloned());
    frame.sheet = crate::dock::sheets::prepare(tree, ui, frame, now);
    let mut regions = frame::regions(area, demand(&draft, &activity, frame.sheet.as_deref(), ui));
    grow(frame, &mut regions, now);
    activity = frame.collect(views::Drawing {
        lines: activity,
        next: activity_next,
    });
    frame.seating = crate::pane::seating(ui, state, regions.transcript);
    regions.transcript = frame.seating.transcript;
    pull_activity(state, ui, frame, regions.composer);
    let layer_regions = regions;
    if crate::browse::active(ui) && regions.transcript.height > 0 {
        regions.transcript.y += 1;
        regions.transcript.height -= 1;
    }
    frame.regions = regions;
    crate::dock::sheets::seat(tree, ui, frame);
    render_transcript(tree, ui, frame, regions.transcript, now, &tools);
    render_activity(ui, frame, regions.activity, activity);
    crate::component::band::place(&band, frame, regions.activity, band_offset);
    render_composer(tree, ui, frame, regions.composer, now, draft);
    crate::pane::draw(ui, state, frame, now);

    if matches!(ui.focus, crate::focus::Focus::Pane(_)) {
        frame.cursor = None;
    }
    render_pill(ui, frame, regions.transcript);
    scrollbar(ui, frame, regions.transcript, now);
    layers(tree, ui, frame, layer_regions, now, &tools);
    render_status(tree, ui, frame, regions.status, now);
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
fn demand(draft: &Draft, activity: &[Line<'_>], sheet: Option<&[Line<'_>]>, ui: &Ui) -> Demand {
    Demand {
        composer: match sheet {
            Some(lines) => u16::try_from(lines.len()).unwrap_or(u16::MAX),
            None => search::demand(ui).unwrap_or_else(|| {
                u16::try_from(draft.rows() + draft.above.len()).unwrap_or(u16::MAX)
            }),
        },
        activity: if sheet.is_some() {
            0
        } else {
            u16::try_from(activity.len()).unwrap_or(u16::MAX)
        },
    }
}

struct Draft {
    layout: prompt::Layout,
    above: Vec<Line<'static>>,
}
impl Draft {
    fn rows(&self) -> usize {
        self.layout.lines.len().clamp(1, COMPOSER_ROWS)
            + usize::from(self.layout.lines.len() > COMPOSER_ROWS)
    }
}

/// What rides in the dock over the input: the open palette's list, or the
/// session's own rows.
fn composer_above(tree: &Tree, ui: &Ui, frame: &mut Layout, now: Now) -> Vec<Line<'static>> {
    if crate::palette::active(ui) {
        let (width, rows) = crate::palette::room(frame.area());
        return crate::palette::draw(ui, tree, frame, width, rows, now);
    }
    let width = frame.area().width as usize;
    let mut drawing = activity::dock(tree, ui, width, now);
    drawing.append(match crate::mention_list::query(ui) {
        Some(query) => mention_list(tree, ui, query, frame.area(), now),
        None => crate::queue_rows::draw(tree.viewed(), &ui.queue_arrivals, width, now),
    });
    let lines = frame.collect(drawing);
    if ui.focus.search().is_some() {
        Vec::new()
    } else {
        lines
    }
}

/// The `@` list in place of the queue, in the dock's room.
fn mention_list(tree: &Tree, ui: &Ui, query: &str, area: Rect, now: Now) -> views::Drawing {
    let offers = crate::mention_list::offered(ui, tree);
    let view = crate::mention_list::draw(
        query,
        &offers,
        ui.menu.selected,
        crate::palette::room(area),
        theme::glyphs::set(),
    );
    views::marked(&view, area.width.into(), &views::Marks::at(now))
}

/// Persistent session facts keep their row in every focus state.
fn render_status(tree: &Tree, ui: &Ui, frame: &mut Layout, area: Rect, now: Now) {
    let area = crate::pane::status(ui, tree.viewed(), frame, area);
    let drawing = crate::footer::draw(tree, ui, frame, area.width as usize, now);
    frame.place(drawing, area);
}

/// The activity row and whatever is queued behind it. It takes the tree
/// rather than the session on screen: between turns the row says what the
/// sessions *around* this one are still doing (M82).
fn render_activity(_: &Ui, frame: &mut Layout, area: Rect, lines: Vec<Line<'static>>) {
    if area.height > 0 {
        frame.render_widget(Paragraph::new(lines), area);
    }
}

fn grow(frame: &mut Layout, regions: &mut Regions, now: Now) {
    let growth = frame
        .growth
        .unwrap_or_else(|| crate::dock::Growth::settled(regions.composer.height, now))
        .resize(regions.composer.height, now);
    let rows = growth.shown(now).min(regions.composer.height);
    let extra = regions.composer.height - rows;
    regions.composer.y += extra;
    regions.composer.height = rows;
    regions.activity.y += extra;
    regions.transcript.height += extra;
    frame.growth = Some(growth);
    frame.wake(growth.next(now));
}
fn render_pill(ui: &Ui, frame: &mut Layout, area: Rect) {
    if ui.scroll.following() && ui.focus.detail().is_none() && ui.focus.search().is_none()
        || area.height == 0
    {
        return;
    }
    let below = frame
        .height
        .saturating_sub(frame.top + usize::from(area.height));
    if below == 0 {
        return;
    }
    if let Some(pill) = pill::pill(below, area.width as usize) {
        let row = Rect {
            y: area.bottom() - 1,
            height: 1,
            ..area
        };
        frame.foot = Some(Rect::new(area.x + pill.x, row.y, pill.width, 1));
        frame.render_widget(Clear, row);
        frame.render_widget(Paragraph::new(pill.line), row);
    }
}
fn scrollbar(ui: &Ui, frame: &mut Layout, area: Rect, now: Now) {
    let since = ui.scroll.touched();
    let below = frame
        .height
        .saturating_sub(frame.top + area.height as usize);
    if since.is_none()
        && (below == 0
            || ui.scroll.following() && ui.focus.detail().is_none() && ui.focus.search().is_none())
    {
        return;
    }
    let h = usize::from(area.height);
    if area.width == 0 || frame.height <= h {
        return;
    }
    let size = ((h * h) as f64 / frame.height as f64).round().max(1.0) as usize;
    let top = (frame.top as f64 / (frame.height - h) as f64 * (h - size) as f64).round() as u16;
    let fresh = since.is_some_and(|since| now.since(since) < crate::scroll::FLASH);
    if below == 0 && !fresh {
        return;
    }
    let ink = theme::foreground(if fresh { "dim" } else { "faint" }).unwrap_or_default();
    for y in top..(top + size as u16).min(area.height) {
        if let Some(cell) = frame.buffer_mut().cell_mut((area.right() - 1, area.y + y)) {
            cell.set_symbol("▕").set_style(ink);
        }
    }
}

/// What floats over the frame, in the order it is stacked: a command's block
/// rides just above the input box; a card or a sheet dims the
/// world and takes the screen. Every one of them is a layer, not a row — the
/// input box never moves to make room.
fn layers(
    tree: &Tree,
    ui: &Ui,
    frame: &mut Layout,
    regions: Regions,
    now: Now,
    tools: &crate::tool_rows::Looks,
) {
    let above = regions.above();
    match ui.focus.drawn(now) {
        Some(reveal) => match crate::browse::reader::current(ui) {
            Some(reader) => crate::browse::reader::draw(
                tree,
                frame,
                above,
                reader,
                reveal,
                tools,
                matches!(ui.focus.open(), Open::Browse(_)),
            ),
            None => layer(tree, ui, frame, above, now),
        },
        None if signing_in(tree) && !crate::sign_in::docked(tree, ui) => {
            card(tree, ui, frame, regions, now)
        }
        None => {}
    }
}

/// Only a sign-in keeps a card; every other ask opens in the dock.
fn signing_in(tree: &Tree) -> bool {
    tree.open_interaction()
        .is_some_and(|(_, open)| matches!(open.kind, bingo_sdk::InteractionKind::Login { .. }))
}

/// The layer a person opened: a sheet over the whole of the frame, or the
/// switcher's card above the input box.
fn layer(tree: &Tree, ui: &Ui, frame: &mut Layout, above: Rect, now: Now) {
    match ui.focus.open() {
        Open::Nothing
        | Open::Reader(_)
        | Open::Keys(_)
        | Open::Mode(_)
        | Open::Result { .. }
        | Open::Rewind(_)
        | Open::Sessions(_)
        | Open::Plugins(_) => {}
        Open::Browse(_) => {
            let lines = crate::browse::draw(
                ui,
                tree,
                frame,
                above.width,
                above.height.min(frame.canvas.area().height / 2),
                now,
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
        // Drawn in the dock over the input.
        Open::Palette(_) => {}
    }
}

/// A sheet over a dimmed frame.
pub(crate) fn sheet(
    frame: &mut Layout,
    above: Rect,
    lines: Vec<Line<'static>>,
    reveal: layers::Reveal,
) {
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
    let drawing = crate::component::sealed::Site {
        state: owner,
        ui,
        now,
        interaction,
        agent: agent.as_deref(),
        width,
        room: room + 1,
    }
    .rows(&mut frame.pulls, opening(interaction, now).moving());
    let mut drawing = frame.collect(drawing);
    let rows = wrapped_card_rows(std::mem::take(&mut drawing.rows), width);
    let rows = fitted_answers(rows, ui.dialog.focus, room);
    let lines: Vec<Line<'static>> = rows.iter().map(|(line, _)| line.clone()).collect();
    // Only a row of the transcript on screen can anchor it: a child's item
    // ids are its own, and would name the wrong row here.
    let anchor = (!asked_elsewhere)
        .then(|| asking_row(frame, interaction))
        .flatten();
    let at = layers::under(above, anchor, shape.height(lines.len(), above));
    place_card(frame, drawing, rows, at, shape.inset(), room);
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

fn place_card(
    frame: &mut Layout,
    mut drawing: crate::component::sealed::Drawing,
    rows: Vec<(Line<'static>, Option<usize>)>,
    at: Rect,
    inset: u16,
    room: usize,
) {
    drawing.place(Rect::new(
        at.x + inset,
        at.y + 1,
        at.width.saturating_sub(2 * inset),
        room as u16,
    ));
    frame.card_actions = drawing.actions;
    frame.card = Some(Card {
        area: at,
        options: rows.iter().map(|(_, option)| *option).collect(),
        sealed_options: drawing.options,
    });
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
fn render_transcript(
    tree: &Tree,
    ui: &Ui,
    frame: &mut Layout,
    area: Rect,
    now: Now,
    tools: &crate::tool_rows::Looks,
) {
    let measurements = std::mem::take(&mut frame.measurements);
    let folds = crate::browse::folds(ui, tree);
    let mut rows = transcript::Rows::of(
        tree.viewed(),
        area.width as usize,
        &folds,
        &measurements,
        &ui.linked,
        now,
    )
    .in_tree(tree)
    .components(&ui.placed.pulled)
    .saying(ui.welcome.update.as_deref())
    .measuring(ui.measure);
    rows.entrance.started_at_ms = ui.welcome.started_at_ms;
    rows.entrance.selected = if let crate::focus::Focus::Welcome(index) = ui.focus {
        Some(index)
    } else {
        None
    };
    rows.tools = tools.clone();
    rows.stop_asked = ui.stop_asked.as_ref();
    let browse = crate::browse::current(ui).filter(|_| crate::browse::active(ui));
    rows.opened.browse_hit = browse
        .as_ref()
        .and_then(|props| props.hit.as_ref())
        .map(|hit| &hit.item);
    rows.opened.detail = crate::turns::detail(tree.viewed(), ui.focus.detail());
    rows.opened.wells = Some(&ui.wells);
    draw_stream(tree, ui, frame, area, &rows);
    frame.measurements = measurements;
}

fn draw_stream(tree: &Tree, ui: &Ui, frame: &mut Layout, area: Rect, rows: &transcript::Rows<'_>) {
    welcome::render(rows, frame, area);
    frame.blocks.sync(rows.state, &tree.agents(), rows);
    if area.height == 0 {
        return;
    }
    transcript_window(tree, ui, frame, area.height as usize, rows.now);
    refresh_stream(tree, ui, frame, area, rows);
    frame.wake(stream_deadline(frame, area.height as usize));
    component_requests(ui, frame, area, rows);
    crate::component::live::requests(rows, frame, area.height);
    crate::component::closing::requests(rows.state, ui, frame, area.width, area.height, rows.now);
    let mut shown = frame.blocks.window(frame.top, area.height as usize);
    let padding = prepare_lines(ui, tree, frame, &mut shown);
    frame.cells = graphics::placed::cells(&shown, area);
    frame.render_widget(Paragraph::new(shown), area);
    mark_transcript(ui, frame, area, padding, rows.now);
    draw_sparks(rows.state, frame, area, rows.now);
}

fn refresh_stream(
    tree: &Tree,
    ui: &Ui,
    frame: &mut Layout,
    area: Rect,
    rows: &transcript::Rows<'_>,
) {
    let height = frame.blocks.height();
    for range in frame.visible_lines(area.height as usize) {
        frame
            .blocks
            .refresh(rows.state, &tree.agents(), rows, range.start, range.len());
    }
    if frame.blocks.height() != height {
        transcript_window(tree, ui, frame, area.height as usize, rows.now);
    }
}

fn stream_deadline(frame: &Layout, height: usize) -> Option<std::time::Instant> {
    frame
        .visible_lines(height)
        .into_iter()
        .filter_map(|range| frame.blocks.next_change(range.start, range.len()))
        .min()
}

fn draw_sparks(state: &SessionState, frame: &mut Layout, area: Rect, now: Now) {
    if let Some(row) = transcript::sparks::anchor(state)
        .and_then(|id| frame.blocks.span(id))
        .and_then(|(line, _)| frame.row_of(line))
    {
        let drawing = transcript::sparks::draw(state, now, frame.buffer_mut(), area, row);
        frame.wake(drawing.next);
    }
}

fn transcript_window(tree: &Tree, ui: &Ui, frame: &mut Layout, rows: usize, now: Now) {
    frame.height = frame.blocks.height() + usize::from(!crate::welcome::blank(tree.viewed()));
    frame.top = ui.scroll.top(frame.height, rows, now.instant);
    if matches!(ui.focus, crate::focus::Focus::Detail(_)) {
        frame.top = crate::detail::top(ui, frame, rows);
    }
    if let Some(search) = ui.focus.search()
        && let Some(top) = search::top(search, frame, rows)
    {
        frame.top = top;
    }
    if let Some(top) = crate::browse::top(ui, tree, frame, rows.saturating_sub(1)) {
        frame.top = top;
    }
}

fn prepare_lines(ui: &Ui, tree: &Tree, frame: &Layout, shown: &mut Vec<Line<'static>>) -> usize {
    crate::browse::mark(ui, tree, frame, shown);
    mark_row(ui, frame, shown);
    if !crate::welcome::blank(tree.viewed()) && frame.top + shown.len() < frame.height {
        shown.push(Line::default());
    }
    let padding = frame.padding();
    shown.splice(0..0, std::iter::repeat_n(Line::default(), padding));
    padding
}

fn mark_transcript(ui: &Ui, frame: &mut Layout, area: Rect, padding: usize, now: Now) {
    // A mark is measured from the row line `top` was drawn at, which is where
    // the lines start rather than where the region does.
    let lined = lined(area, padding);
    if let Some(search) = ui.focus.search() {
        let lines = search::drawn(frame);
        search::mark(&mut frame.canvas, lined, frame.top, search, &lines);
    }
    if let Some(run) = ui.focus.selection().as_ref() {
        select::mark(&mut frame.canvas, lined, frame.top, run);
    }
    if ui.crossfading(now) {
        layers::hush(frame, area);
    }
}

fn component_requests(ui: &Ui, frame: &mut Layout, area: Rect, rows: &transcript::Rows<'_>) {
    let mut keys = Vec::new();
    let visible = frame.visible_lines(area.height as usize);
    for (id, span) in frame.blocks.spans() {
        if !visible
            .iter()
            .any(|range| span.end > range.start && span.start < range.end)
        {
            continue;
        }
        let Some(item) = rows.state.transcript.get(id) else {
            continue;
        };
        if let Some((id, props)) = transcript::band::request(item, rows) {
            keys.extend(crate::component::key(
                rows.state,
                &id,
                &props,
                area.width,
                area.height,
            ));
        }
        let role = frame.blocks.roles().of_item(id);
        if role == &crate::turns::Role::Own {
            keys.extend(item_requests(item, ui, area, rows));
        } else {
            keys.extend(
                crate::turns::requests(item, role, rows)
                    .into_iter()
                    .filter_map(|(id, props)| {
                        crate::component::key(
                            rows.state,
                            &id,
                            &props,
                            if matches!(props, bingo_sdk::component::Props::ToolResult(_)) {
                                crate::well::inner(area.width as usize) as u16
                            } else {
                                area.width
                            },
                            area.height,
                        )
                    }),
            );
        }
    }
    frame.pulls.extend(keys);
}

fn item_requests(
    item: &bingo_sdk::Item,
    ui: &Ui,
    area: Rect,
    rows: &transcript::Rows<'_>,
) -> Vec<crate::pulled::Key> {
    let mut keys = Vec::new();
    for receipt in rows.answers.to(&item.id) {
        if let Some(props) = crate::component::items::props(receipt, false) {
            keys.extend(crate::component::key(
                rows.state,
                &crate::component::items::id(receipt),
                &props,
                area.width,
                area.height,
            ));
        }
    }
    let expanded = crate::fold::fold_of(&ui.folds, item) == crate::fold::Fold::Open;
    for props in crate::component::items::all(item, expanded) {
        if let Some(key) = crate::component::key(
            rows.state,
            &crate::component::items::id(item),
            &props,
            area.width,
            area.height,
        ) {
            let hides_children = matches!(
                props,
                bingo_sdk::component::Props::ToolGroup(_) | bingo_sdk::component::Props::ToolUse(_)
            ) && ui
                .placed
                .pulled
                .display(&key)
                .is_some_and(|view| !crate::component::has_engine(view, props.component()));
            keys.push(key);
            if hides_children {
                break;
            }
        }
    }
    keys
}

/// Mark focus in the drawn copy, leaving the cached transcript unchanged.
fn mark_row(ui: &Ui, frame: &Layout, shown: &mut [Line<'static>]) {
    let Some(item) = ui.focus.detail().or_else(|| ui.focus.row()) else {
        return;
    };
    let Some((start, _)) = frame.blocks.span(item) else {
        return;
    };
    let Some(row) = frame
        .row_of(start)
        .and_then(|at| usize::from(at).checked_sub(frame.padding()))
        .and_then(|at| shown.get_mut(at))
    else {
        return;
    };
    if ui.focus.detail().is_some() {
        let ground = theme::ground("pickBg").unwrap_or_default();
        *row = crate::layout::grid::pad_to(row.clone(), frame.regions.transcript.width as usize);
        row.style = ground;
        for span in &mut row.spans {
            span.style = ground.patch(span.style);
        }
        return;
    }
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
fn render_composer(tree: &Tree, ui: &Ui, frame: &mut Layout, area: Rect, now: Now, draft: Draft) {
    if let Some(lines) = frame.sheet.clone() {
        crate::dock::sheets::draw(frame, area, lines);
        let edge = crate::dock::draw(tree, ui, area.width as usize, now);
        frame.place(
            edge,
            Rect {
                height: area.height.min(1),
                ..area
            },
        );
        return;
    }
    frame.render_widget(Paragraph::new("").style(crate::dock::ground()), area);
    let drawing = crate::dock::draw(tree, ui, area.width as usize, now);
    frame.place(
        drawing,
        Rect {
            height: area.height.min(1),
            ..area
        },
    );
    let inner = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(2),
        ..area
    };
    if let Some(search) = ui.focus.search() {
        search::draw(search, ui, frame, inner, now);
    } else {
        let rows = draft.rows();
        let above = inner
            .height
            .saturating_sub(rows as u16)
            .min(draft.above.len() as u16);
        let start = draft.above.len().saturating_sub(above as usize);
        frame.render_widget(
            Paragraph::new(draft.above.into_iter().skip(start).collect::<Vec<_>>()),
            Rect {
                height: above,
                ..inner
            },
        );
        let input = Rect {
            y: inner.y + above,
            height: inner.height - above,
            ..inner
        };
        render_draft(tree.viewed(), ui, frame, input, draft.layout, now);
    }
}

/// The draft itself and the caret in it. `width` is the box's, borders and
/// all, which is what says how wide a row of text may be.
fn render_draft(
    state: &SessionState,
    ui: &Ui,
    frame: &mut Layout,
    area: Rect,
    layout: prompt::Layout,
    now: Now,
) {
    if usize::from(area.width) <= crate::layout::grid::TX {
        return;
    }
    let props = crate::keybar::composer(state, ui, frame);
    let overflow =
        usize::from(layout.lines.len() > COMPOSER_ROWS && area.height as usize > COMPOSER_ROWS);
    let count = area
        .height
        .saturating_sub(overflow as u16)
        .min(COMPOSER_ROWS as u16)
        .min(u16::try_from(layout.lines.len()).unwrap_or(u16::MAX));
    let prefix = prompt::COLUMN.saturating_add(props.tag.width());
    let x = area
        .x
        .saturating_add(u16::try_from(prefix).unwrap_or(u16::MAX));
    frame.composer_text = Some(Rect {
        x,
        y: area.y.saturating_add(overflow as u16),
        width: area.right().saturating_sub(x),
        height: count,
    });
    let active = matches!(ui.focus, crate::focus::Focus::Composer)
        || matches!(&ui.focus, crate::focus::Focus::Layer(layer) if !layer.showing() || matches!(layer.open, Open::Palette(_)));
    let drawing = prompt::drawing(
        &layout,
        &props,
        ui.composer.is_empty(),
        usize::from(area.height),
        active,
        ui.keyed_at,
        Now {
            motion: now.motion && active,
            ..now
        },
    );
    frame.place(drawing, area);
    if active {
        frame.cursor = composer_cursor(area, &layout, &props);
    }
}

fn composer_cursor(
    area: Rect,
    layout: &prompt::Layout,
    props: &bingo_sdk::component::ComposerProps,
) -> Option<ratatui::layout::Position> {
    let row = u16::try_from(layout.cursor.0).ok()?;
    let column = prompt::COLUMN
        .checked_add(props.tag.width())?
        .checked_add(layout.cursor.1)?;
    let column = u16::try_from(column).ok()?;
    let x = area.x.checked_add(column)?;
    let y = area.y.checked_add(row)?;
    (x < area.right() && y < area.bottom()).then_some((x, y).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use bingo_sdk::{
        ContentPart, ContextUsage, Event, InterruptReason, ItemBody, ItemStatus, KernelError,
        Level, Preview, QueueEntry, ToolOutput, TurnId, TurnStatus, View,
    };
    use serde_json::json;
    use unicode_width::UnicodeWidthStr;

    fn render(state: &SessionState, ui: &mut Ui, now: Now) -> String {
        catalogue(ui);
        crate::test_support::render(state, ui, now)
    }

    #[test]
    fn composer_cursor_uses_the_drawn_text_position() {
        let area = Rect::new(10, 5, 30, 3);
        let layout = prompt::Layout {
            lines: vec!["first".into(), "second".into()],
            images: Vec::new(),
            selection: vec![None, None],
            cursor: (1, 4),
        };
        let props = bingo_sdk::component::ComposerProps {
            tag: "@ ".into(),
            placeholder: String::new(),
        };
        assert_eq!(
            super::composer_cursor(area, &layout, &props),
            Some((19, 6).into())
        );
    }

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
        let now = still(now);
        insta::assert_snapshot!(render(&state, &mut ui, now));
    }

    #[test]
    fn a_completed_tool_keeps_its_detail_out_of_the_stream() {
        let output = ToolOutput {
            parts: vec![ContentPart::text(
                (1..=9).map(|i| format!("line {i}\n")).collect::<String>(),
            )],
            is_error: false,
            display: None,
            upshot: None,
        };
        // Direct invocation also starts a turn before publishing its call.
        let state = folded(vec![
            frame(1, started("trn_1")),
            item_frame(
                2,
                tool(
                    "itm_1",
                    "Read",
                    json!({"file_path": "src/lib.rs"}),
                    Some(output),
                    ItemStatus::Completed,
                ),
            ),
            frame(3, completed("trn_1", TurnStatus::Completed)),
        ]);
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
    fn permission_with_the_feedback_row_open() {
        let state = folded(vec![frame(
            1,
            opened(permission(
                None,
                Some(Preview::Command {
                    command: "rm -rf build".into(),
                    description: None,
                    stakes: None,
                    cwd: "/tmp/project".into(),
                }),
            )),
        )]);
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        let now = Now {
            wall: now.wall + crate::ask::guard::GUARD,
            ..now
        };
        write(&mut ui, &state, "use cargo clean", now);
        insta::assert_snapshot!(render(&state, &mut ui, now));
    }

    #[test]
    fn an_ask_in_the_dock_does_not_dim_the_conversation() {
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
        assert!(render(&state, &mut ui, now).contains("No, and tell it why"));
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
        assert!(mine.contains("also fix the docs"), "{mine}");

        let steer = band(&[("room", "the build is green", true)]);
        assert_eq!(
            steer.lines().take(23).collect::<Vec<_>>(),
            band(&[]).lines().take(23).collect::<Vec<_>>(),
            "a steer in flight draws nothing at all, not even the blank row it \
             would have been spaced from the transcript by"
        );

        let both = band(&[
            ("room", "the build is green", true),
            ("tui", "also fix the docs", true),
        ]);
        assert!(both.contains("also fix the docs"), "{both}");
        assert!(!both.contains("the build is green"), "{both}");

        assert_eq!(
            band(&[("brand-new", "who knows", true)])
                .lines()
                .take(23)
                .collect::<Vec<_>>(),
            band(&[]).lines().take(23).collect::<Vec<_>>(),
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
        assert!(steers.contains("also fix the docs"), "{steers}");
        assert!(steers.contains("at the next step"), "{steers}");

        let waits = band(&[("tui", "and then the docs", false)]);
        assert!(
            waits.contains("and then the docs") && waits.contains("queued"),
            "{waits}"
        );

        assert!(waits.contains("Steer, or queue what comes next"), "{waits}");
        assert!(
            !waits.contains("Ask anything, or / for commands"),
            "{waits}"
        );
        let idle = render(&state(), &mut scene().0, scene().1);
        assert!(idle.contains("Ask anything, or / for commands"), "{idle}");
        assert!(idle.contains("   ▍Ask anything"), "{idle}");
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
            activity::components(
                &solo(&folded(vec![
                    frame(1, started("trn_1")),
                    queue_frame(2, entries),
                ])),
                &ui,
                80,
                now,
            )
            .lines
            .len()
        };
        assert_eq!(rows(&[("room", "the build is green", true)]), 0);
        assert_eq!(
            rows(&[("tui", "also fix the docs", true)]),
            0,
            "queued rows belong to the dock"
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
                        asked_at: None,
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
    fn a_result_sheet_replaces_the_composer() {
        let state = state();
        let (mut ui, now) = scene();
        ui.focus.show(
            Open::Result {
                head: String::new(),
                at: 0,
                view: View::Table {
                    headers: vec!["mode".into(), "meaning".into()],
                    rows: vec![
                        vec!["default".into(), "ask for what is not allowed".into()],
                        vec![
                            "acceptEdits".into(),
                            "edits inside the cwd are allowed".into(),
                        ],
                    ],
                },
            },
            now.instant,
        );
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
                        upshot: None,
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
        assert!(screen.contains("spawnagent  review the diff"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn a_child_that_needs_a_person_is_counted_on_the_status_line() {
        let tree = spawned(vec![child_frame(2, opened(child_permission()))]);
        let (mut ui, now) = settled();
        ui.dialog
            .focus_on(tree.open_interaction().map(|(_, open)| open));
        let screen = render_tree(&tree, &mut ui, now);
        assert!(screen.contains("◆ reviewer needs you"), "{screen}");
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
        let (mut running_ui, running_now) = mid_turn();
        let running = render_tree(&tree, &mut running_ui, running_now);
        assert!(running.contains("Steer reviewer"), "{running}");
        tree.fold
            .apply(&child_frame(10, completed("trn_9", TurnStatus::Completed)));
        let (mut ui, now) = mid_turn();
        let screen = render_tree(&tree, &mut ui, now);
        assert!(screen.contains("Message reviewer"), "{screen}");
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
        assert!(screen.contains("▎  reviewer   the plan"), "{screen}");
        assert!(
            screen.contains("▎             none of M9's exit criteria name one"),
            "the rest of a message is prose under the name, not a folded \
             result: {screen}"
        );
        assert!(!screen.contains("⎿"), "{screen}");
        assert!(screen.contains("▎  scout      M9's"), "{screen}");
        assert!(
            screen.contains("   you        then let us ship it"),
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
        assert!(
            screen.contains("Post to #design, @ to call on someone"),
            "{screen}"
        );
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

    /// A finished step keeps its display in detail until it is opened.
    #[test]
    fn a_finished_display_stays_closed_until_opened() {
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
            upshot: None,
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
        assert!(!screen.contains("write the plan"), "{screen}");
        insta::assert_snapshot!(screen);
        crate::detail::toggle(&mut ui, &state, &bingo_sdk::ItemId::from_raw("itm_1"));
        let screen = render(&state, &mut ui, now);
        assert!(screen.contains("     id   task"), "{screen}");
        assert!(!screen.contains("2 open"), "the model's text is not drawn");
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
        assert!(typing.contains("find  line 4"), "{typing}");

        // The transcript eases to the hit; this is where it lands.
        let there = Now {
            instant: now.instant + crate::scroll::EASE,
            ..now
        };
        let found = render(&state, &mut ui, there);
        assert!(found.contains("11 of 11"), "{found}");
        assert!(found.contains("line 4"), "it scrolled to the hit: {found}");
        insta::assert_snapshot!(found);

        press(&mut ui, key(crossterm::event::KeyCode::Up));
        let stepped = render(&state, &mut ui, there);
        assert!(stepped.contains("10 of 11"), "{stepped}");

        press(&mut ui, key(crossterm::event::KeyCode::Esc));
        assert!(
            render(&state, &mut ui, there).contains("fake-1"),
            "esc gives the status line back"
        );
    }

    #[test]
    fn a_hit_on_the_screen_is_marked_where_it_sits() {
        crate::render::Render {
            look: crate::painted::truecolor(),
            ..Default::default()
        }
        .scope(|| {
            let state = long_transcript(60);
            let (mut ui, now) = scene();
            render(&state, &mut ui, now);
            crate::input::on_key(&mut ui, &solo(&state), ctrl('f'), now);
            for c in "line 59".chars() {
                crate::input::on_key(&mut ui, &solo(&state), typed(c), now);
            }

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
        });
    }

    /// The cells carrying the mark a hit is drawn in.
    fn hit_cells(screen: &ratatui::backend::TestBackend) -> Vec<(u16, u16)> {
        screen
            .buffer()
            .content()
            .iter()
            .enumerate()
            .filter(|(i, cell)| {
                *i >= 160
                    && cell.style()
                        == theme::as_drawn(
                            theme::ground("glow")
                                .unwrap()
                                .patch(theme::foreground("bg").unwrap()),
                        )
            })
            .map(|(i, _)| ((i % 80) as u16, (i / 80) as u16))
            .collect()
    }

    /// A hit is marked the way a run is tinted: on a transcript shorter than
    /// its region, both hang with the lines and not with the pane.
    #[test]
    fn a_hit_on_a_short_transcript_is_marked_where_it_sits() {
        crate::render::Render {
            look: crate::painted::truecolor(),
            ..Default::default()
        }
        .scope(|| {
            let state = folded(vec![item_frame(1, user("itm_1", "run the tests"))]);
            let tree = solo(&state);
            let (mut ui, now) = scene();
            let row = row_carrying(&render(&state, &mut ui, now), "run the tests");
            crate::input::on_key(&mut ui, &tree, ctrl('f'), now);
            for c in "tests".chars() {
                crate::input::on_key(&mut ui, &tree, typed(c), now);
            }

            assert_eq!(
                hit_cells(&drawn(80, 24, &tree, &mut ui, now)),
                (11..16).map(|x| (x, row)).collect::<Vec<_>>(),
                "the five cells of `tests` on the row it is drawn on"
            );
        });
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
        let state = crate::test_support::session(
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

    #[test]
    fn images_stay_in_the_input_at_every_terminal_depth() {
        for graphics in [crate::graphics::Graphics::Off, crate::graphics::drawing()] {
            crate::graphics::with(graphics, || {
                let (mut ui, now) = scene();
                let state = state();
                render(&state, &mut ui, now);
                let region = ui.shown.regions.composer;
                for _ in 0..5 {
                    drafted(&mut ui, bingo_pictures::testing::png(40, 40));
                }
                let screen = render(&state, &mut ui, now);
                assert!(screen.contains("[image 5]"));
                assert!(!screen.contains(crate::graphics::kitty::PLACEHOLDER));
                assert_eq!(region, ui.shown.regions.composer);
                assert!(ui.shown.cells.is_empty());
                ui.composer.clear();
                assert!(!render(&state, &mut ui, now).contains("[image 1]"));
            });
        }
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

    /// A person's measure narrows the region; prose keeps its own 100-cell cap.
    #[test]
    fn prose_keeps_its_measure_inside_a_persons_narrower_region() {
        let state = folded(vec![item_frame(
            1,
            assistant("itm_1", &"word ".repeat(80), ItemStatus::Completed),
        )]);
        let tree = solo(&state);
        let (mut ui, now) = scene();
        let filled = widest_prose(&drawn(200, 40, &tree, &mut ui, now));
        assert_eq!(filled, 102);
        ui.measure = Some(60);
        let capped = widest_prose(&drawn(200, 40, &tree, &mut ui, now));
        assert_eq!(capped, 57);
    }

    /// The frame yields the strip before it yields the row a person is
    /// typing on: a screen too short for both keeps the prompt.
    #[test]
    fn a_screen_too_short_for_both_keeps_the_prompt_and_not_the_strip() {
        let state = state();
        let (mut ui, now) = scene();
        crate::graphics::with(crate::graphics::drawing(), || {
            drafted(&mut ui, bingo_pictures::testing::png(40, 40));
            let screen = draw_sized(80, 5, &state, &mut ui, now);
            assert!(!screen.contains(crate::graphics::kitty::PLACEHOLDER));
            assert!(screen.contains("[image 1]"), "{screen}");
        });
    }
}

#[cfg(test)]
mod placement_tests {
    use super::*;
    use crate::test_support::{later, scene, still};

    #[test]
    fn the_scrollbar_stays_for_nine_hundred_ms_at_the_end() {
        let (mut ui, now) = scene();
        ui.scroll.end(now.instant);
        let area = Rect::new(0, 0, 80, 10);
        for ms in [0, 899, 900] {
            let mut frame = Layout::default();
            frame.canvas = crate::layout::Canvas::new(area);
            frame.height = 100;
            frame.top = 90;
            scrollbar(&ui, &mut frame, area, still(later(now, ms)));
            assert_eq!(
                frame.buffer.cell((79, 9)).unwrap().symbol() == "▕",
                ms < 900
            );
        }
    }
    #[test]
    fn sending_while_following_does_not_flash_the_scrollbar() {
        use crate::test_support::*;
        let (mut ui, now) = scene();
        let state = folded(
            (1..=60)
                .map(|i| {
                    frame(
                        i,
                        bingo_sdk::Event::ItemCompleted {
                            item: user(&format!("line-{i}"), &format!("line {i}")),
                        },
                    )
                })
                .collect(),
        );
        let tree = solo(&state);
        draw_tree(80, 24, &tree, &mut ui, still(now));
        assert!(ui.shown.height > usize::from(ui.shown.regions.transcript.height));
        ui.composer.set("next");
        crate::input::on_key(
            &mut ui,
            &tree,
            key(crossterm::event::KeyCode::Enter),
            still(now),
        );
        assert!(!draw_tree(80, 24, &tree, &mut ui, still(now)).contains('▕'));
        crate::input::on_key(
            &mut ui,
            &tree,
            key(crossterm::event::KeyCode::PageUp),
            still(now),
        );
        draw_tree(80, 24, &tree, &mut ui, still(later(now, 100)));
        crate::input::on_key(
            &mut ui,
            &tree,
            key(crossterm::event::KeyCode::End),
            still(later(now, 100)),
        );
        for ms in [100, 999, 1000] {
            assert_eq!(
                draw_tree(80, 24, &tree, &mut ui, still(later(now, ms))).contains('▕'),
                ms < 1000
            );
        }
    }

    #[test]
    fn a_zero_height_footer_keeps_its_countdown_deadline() {
        use crate::test_support::*;
        let (ui, now) = scene();
        let state = folded(vec![frame(
            1,
            extended(
                "clock",
                "wake",
                bingo_sdk::slot::with(
                    serde_json::json!({}),
                    [bingo_sdk::slot::Slot::Status {
                        text: "wakes".into(),
                        until: Some(later(now, 125_500).wall),
                        tone: bingo_sdk::Tone::Neutral,
                    }],
                ),
            ),
        )]);
        let mut layout = Layout::default();
        render_status(
            &solo(&state),
            &ui,
            &mut layout,
            Rect::new(0, 0, 120, 0),
            still(now),
        );
        assert_eq!(
            layout.deadline(),
            Some(later(now, 500).instant + std::time::Duration::from_nanos(1))
        );
    }

    #[test]
    fn a_zero_height_edge_keeps_its_flash_deadline() {
        use crate::test_support::*;
        let (ui, now) = scene();
        let state = folded(vec![
            frame(1, started("turn")),
            frame(2, completed("turn", bingo_sdk::TurnStatus::Completed)),
        ]);
        let mut layout = Layout::default();
        render_composer(
            &solo(&state),
            &ui,
            &mut layout,
            Rect::new(0, 0, 80, 0),
            still(now),
            Draft {
                layout: ui.composer.layout(74),
                above: Vec::new(),
            },
        );
        assert_eq!(layout.deadline(), Some(later(now, 1100).instant));
    }
}

#[cfg(test)]
mod part_deadlines;
