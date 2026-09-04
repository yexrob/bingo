//! The one list of sessions, as a keyboard: what `ctrl+g` and `↓` on an empty
//! box open, what the arrows walk, what a query narrows, and what `⏎` and `esc`
//! settle.
//!
//! Nothing here is state. [`Switcher`] holds where the cursor is, what the
//! store answered with and where the gesture started; the list itself is
//! composed from the tree at every key, so a keypress and a click cannot be
//! reading two different lists.

use bingo_sdk::SessionId;
use crossterm::event::{KeyCode, KeyEvent};

use crate::clock::Now;
use crate::effect::Effect;
use crate::roster;
use crate::tree::{self, Tree};
use crate::ui::{Open, Switcher, Ui};

/// `↓` on an empty composer is the list's other door — but only where there is
/// somewhere to go. Alone in the tree the key keeps the meaning it has always
/// had rather than putting up a list of one.
pub(super) fn opens(ui: &Ui, tree: &Tree) -> bool {
    ui.composer.is_empty() && tree.rows().len() > 1
}

/// Open the list on the session in view, or close it again. What this
/// attachment carries is on the list at once; what is only in the store lands
/// when the read the opening spawns comes back, which is why it goes up even
/// where the tree is the root alone.
pub(super) fn toggle(ui: &mut Ui, tree: &Tree, now: Now) -> Vec<Effect> {
    if ui.layer.showing() {
        ui.layer.close(now.instant);
        return Vec::new();
    }
    let rows = tree::roster(tree, &[]);
    ui.layer.show(
        Open::Switcher(Switcher {
            cursor: roster::Cursor::on(&roster::listing(tree, &rows, ""), tree.view()),
            query: String::new(),
            stored: Vec::new(),
            // Where the walk started, so `esc` can put it back.
            from: Some(tree.view().clone()),
        }),
        now.instant,
    );
    vec![Effect::ListStored]
}

/// The list owns the keyboard while it is up: `↑`/`↓` walk the one column,
/// labels and all, and the view goes with the cursor — walking the list *is*
/// the switch, as the strip's walk was (§3). A printable key narrows the list
/// instead (M55) and moves the cursor the same way. `⏎` settles on where the
/// walk landed; `esc` takes the query back, and with none gives back where the
/// list was opened from.
pub(super) fn keys(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    if let Some((cursor, chosen)) = walked(ui, tree, key) {
        return walk_to(ui, tree, cursor, chosen);
    }
    match queried(ui, key) {
        Some(query) => narrow(ui, tree, query),
        None => settle(ui, tree, key, now),
    }
}

/// Where the arrows move the cursor, and the session that lands under it.
/// `None` says the key was not one of the list's own.
fn walked(ui: &Ui, tree: &Tree, key: KeyEvent) -> Option<(roster::Cursor, Option<SessionId>)> {
    let Open::Switcher(open) = &ui.layer.open else {
        return None;
    };
    let rows = tree::roster(tree, &open.stored);
    let listing = roster::listing(tree, &rows, &open.query);
    let cursor = match key.code {
        KeyCode::Up => open.cursor.step(&listing, -1),
        KeyCode::Down => open.cursor.step(&listing, 1),
        _ => return None,
    };
    Some((cursor, cursor.row(&listing).map(|row| row.session.clone())))
}

/// What a key leaves the query as: a printable character appends to it,
/// backspace takes one back. `None` says the key was not the query's — an
/// empty query has no backspace to answer, so `esc esc` is not stolen from it.
fn queried(ui: &Ui, key: KeyEvent) -> Option<String> {
    let Open::Switcher(open) = &ui.layer.open else {
        return None;
    };
    let mut query = open.query.clone();
    match key.code {
        KeyCode::Char(c) => query.push(c),
        KeyCode::Backspace if !query.is_empty() => {
            query.pop();
        }
        _ => return None,
    }
    Some(query)
}

/// The list once the query is this. The cursor keeps the session it was on
/// where the query left that row on the list, and takes the first row there is
/// where it did not; the view follows the cursor as it does on a walk, so `⏎`
/// keeps what a person is looking at.
fn narrow(ui: &mut Ui, tree: &Tree, query: String) -> Vec<Effect> {
    let Open::Switcher(open) = &ui.layer.open else {
        return Vec::new();
    };
    let was = open.session(tree, open.cursor);
    if let Open::Switcher(open) = &mut ui.layer.open {
        open.query = query;
    }
    let (cursor, chosen) = placed(ui, tree, was.as_ref());
    walk_to(ui, tree, cursor, chosen)
}

/// Where the cursor sits on the list as it stands, for the session it was on:
/// that row where the query left it, and the first row where it did not.
fn placed(ui: &Ui, tree: &Tree, was: Option<&SessionId>) -> (roster::Cursor, Option<SessionId>) {
    let Open::Switcher(open) = &ui.layer.open else {
        return (roster::Cursor::default(), None);
    };
    let rows = tree::roster(tree, &open.stored);
    let listing = roster::listing(tree, &rows, &open.query);
    let cursor = was
        .map(|id| roster::Cursor::on(&listing, id))
        .unwrap_or_default();
    (cursor, cursor.row(&listing).map(|row| row.session.clone()))
}

/// Put the cursor there and show what it names. A session already on screen
/// asks for no switch: the crossfade reports a change of place, and there was
/// none.
pub(crate) fn walk_to(
    ui: &mut Ui,
    tree: &Tree,
    cursor: roster::Cursor,
    chosen: Option<SessionId>,
) -> Vec<Effect> {
    if let Open::Switcher(open) = &mut ui.layer.open {
        open.cursor = cursor;
    }
    chosen
        .filter(|id| id != tree.view())
        .map(|id| vec![Effect::View(id)])
        .unwrap_or_default()
}

/// `⏎` keeps the session the walk landed on — it is already the one on screen,
/// so settling is only closing the list. `esc` is §7's stack: the query a
/// person typed is the first thing it takes back, and the list itself the next,
/// putting back the session it was opened from.
fn settle(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    let Open::Switcher(open) = &ui.layer.open else {
        return Vec::new();
    };
    let opened_from = open.from.clone();
    let queried = !open.query.is_empty();
    match key.code {
        KeyCode::Enter => ui.layer.close(now.instant),
        KeyCode::Esc if queried => return narrow(ui, tree, String::new()),
        KeyCode::Esc => {
            ui.layer.close(now.instant);
            return opened_from
                .map(|id| vec![Effect::View(id)])
                .unwrap_or_default();
        }
        _ => {}
    }
    Vec::new()
}
