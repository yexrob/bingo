//! One pure function from a mouse event to a list of effects, against the
//! frame the last draw left behind.
//!
//! The pointer is the other half of [`crate::input`]: a key is a gesture with
//! one meaning, and a click is a gesture with a place — so where it landed is
//! the whole of this module's work, and what to do there is the same handful
//! of answers the keys give (§7: a key means one direction, a click means
//! both).

use bingo_sdk::{SessionId, SessionState};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::clock::Now;
use crate::effect::Effect;
use crate::fold;
use crate::input::{item_of, scroll, walk_to};
use crate::rail::CardId;
use crate::roster;
use crate::select::Cell;
use crate::tree::Tree;
use crate::ui::{Open, Ui};

/// Lines one notch of the wheel moves the transcript.
pub const WHEEL: isize = 3;

/// One pure function from a mouse event to a list of effects, against the
/// frame the last draw left behind: the wheel scrolls, a drag takes a run of
/// cells, a click lands on a block, on a child's row, or on a card's option.
pub fn on_mouse(ui: &mut Ui, tree: &Tree, mouse: MouseEvent, now: Now) -> Vec<Effect> {
    match mouse.kind {
        MouseEventKind::ScrollUp => scroll(ui, WHEEL, now),
        MouseEventKind::ScrollDown => scroll(ui, -WHEEL, now),
        MouseEventKind::Down(MouseButton::Left) => return pressed(ui, tree, mouse, now),
        MouseEventKind::Drag(MouseButton::Left) => drag(ui, mouse),
        _ => {}
    }
    Vec::new()
}

/// A press lands on whatever is under it: a card's option answers, a block
/// takes the focus, opens what is folded under it and starts a run.
fn pressed(ui: &mut Ui, tree: &Tree, mouse: MouseEvent, now: Now) -> Vec<Effect> {
    if let Some(index) = card_option(ui, mouse) {
        return answer(ui, tree, index, now);
    }
    // A click on a row of the list does what the cursor there does (§7: a key
    // means one direction, a click means both — here it means the walk).
    if let Some(cursor) = listed_row(ui, mouse) {
        let chosen = under(ui, tree, cursor);
        return walk_to(ui, tree, cursor, chosen);
    }
    if let Some(card) = rail_card(ui, mouse) {
        ui.focus = Some(card);
        return Vec::new();
    }
    // A picture is a thing to click before it is part of a block: a click on
    // one opens it, and a click beside it is the block's (§7).
    if let Some(picture) = picture(ui, mouse) {
        return vec![Effect::OpenPicture(picture.source)];
    }
    let Some(cell) = transcript_cell(ui, mouse) else {
        return Vec::new();
    };
    let block = ui.painted.borrow().blocks.at(cell.line);
    // A row that spawned a session is that session's row: `⏎` steps in, and
    // so does a click.
    if let Some(session) = block.as_ref().and_then(|item| tree.spawned_by(item)) {
        return vec![Effect::View(session.clone())];
    }
    if let Some(item) = &block {
        cycle_fold(ui, tree.viewed(), item);
    }
    ui.select.block = block;
    ui.select.start(cell);
    Vec::new()
}

/// A click on a block walks its fold one step and comes back to where its kind
/// starts (§7). A *key* never means two directions (§7, M11e); a click on the
/// same row is one gesture, and one gesture may go round.
///
/// A result, a notice and an action have the two states they always had — open
/// and their five-row cut — because that cut is where they start. A thought
/// that is over starts shut, so the same walk gives it three: shut, its first
/// two rows, the whole of it. Only a thought has a state worth skipping past.
///
/// It writes the map `ctrl+o` writes, so a block is open in one way only.
fn cycle_fold(ui: &mut Ui, state: &SessionState, id: &bingo_sdk::ItemId) {
    let Some(item) = item_of(state, id) else {
        return;
    };
    let next = fold::cycled(item, fold::fold_of(&ui.folds, item));
    ui.folds.insert(id.clone(), next);
}

/// A drag takes the far end of the run with it.
fn drag(ui: &mut Ui, mouse: MouseEvent) {
    if let Some(cell) = transcript_cell(ui, mouse) {
        ui.select.extend(cell);
    }
}

/// The picture under the pointer, wherever this frame drew one — among the
/// transcript's rows, or on the composer's strip. A layer over the frame has
/// covered both, so while one is up the click is the layer's and no picture is
/// under it.
fn picture(ui: &Ui, mouse: MouseEvent) -> Option<crate::graphics::Picture> {
    if ui.layer.showing() {
        return None;
    }
    ui.painted.borrow().picture_at(Position {
        x: mouse.column,
        y: mouse.row,
    })
}

/// Which option of the open card the pointer is on.
fn card_option(ui: &Ui, mouse: MouseEvent) -> Option<usize> {
    let painted = ui.painted.borrow();
    let card = painted.card.as_ref()?;
    let inside = card.area.contains(Position {
        x: mouse.column,
        y: mouse.row,
    });
    let row = usize::from(mouse.row.checked_sub(card.area.y + 1)?);
    inside
        .then(|| card.options.get(row).copied().flatten())
        .flatten()
}

/// Answer the open interaction on the option a click landed on.
fn answer(ui: &mut Ui, tree: &Tree, index: usize, now: Now) -> Vec<Effect> {
    let Some((_, interaction)) = tree.open_interaction() else {
        return Vec::new();
    };
    ui.dialog.focus = index;
    ui.dialog.on_key(interaction, mouse_enter(), now)
}

/// What a click on a card row means: the row it landed on, chosen.
fn mouse_enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
}

/// The row of the list under the pointer, against the frame the last draw
/// left: which line it landed on, and what that line is a row of.
fn listed_row(ui: &Ui, mouse: MouseEvent) -> Option<roster::Cursor> {
    if !ui.layer.captures() {
        return None;
    }
    let painted = ui.painted.borrow();
    painted.list.as_ref()?.at(mouse.row)
}

/// The session a row of the list names — the list as the query left it, so a
/// click lands on the row a person can see.
fn under(ui: &Ui, tree: &Tree, cursor: roster::Cursor) -> Option<SessionId> {
    let Open::Switcher(open) = &ui.layer.open else {
        return None;
    };
    open.session(tree, ui.composer.text(), cursor)
}

/// The rail card under the pointer, when it is over the rail.
fn rail_card(ui: &Ui, mouse: MouseEvent) -> Option<CardId> {
    let painted = ui.painted.borrow();
    let area = painted.regions.rail?;
    if !area.contains(Position {
        x: mouse.column,
        y: mouse.row,
    }) {
        return None;
    }
    let row = usize::from(mouse.row - area.y);
    painted
        .rail
        .iter()
        .find(|(_, rows)| rows.contains(&row))
        .map(|(id, _)| id.clone())
}

/// The transcript cell under the pointer, when it is over the transcript.
fn transcript_cell(ui: &Ui, mouse: MouseEvent) -> Option<Cell> {
    let painted = ui.painted.borrow();
    let region = painted.regions.transcript;
    if !region.contains(Position {
        x: mouse.column,
        y: mouse.row,
    }) {
        return None;
    }
    Some(Cell {
        line: painted.line_at(usize::from(mouse.row - region.y))?,
        column: usize::from(mouse.column - region.x),
    })
}
