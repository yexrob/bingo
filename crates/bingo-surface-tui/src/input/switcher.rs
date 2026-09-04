//! The one list of sessions, as a keyboard: what `ctrl+g` and `↓` on an empty
//! box open, what the arrows walk, what typing narrows, and what `⏎` and `esc`
//! settle.
//!
//! **The query is the line** (M58): what narrows the list is the input box's
//! own text, as it is under the `/` and `@` dropdowns — one rule for every
//! list in the surface. The line a person was writing is set aside while the
//! list is up and put back when it goes.
//!
//! Nothing else here is state. [`Switcher`] holds where the cursor is, that
//! draft, what the store answered with and where the gesture started; the list
//! itself is composed from the tree at every key, so a keypress and a click
//! cannot be reading two different lists.

use bingo_sdk::SessionId;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::clock::Now;
use crate::effect::Effect;
use crate::roster;
use crate::tree::{self, Tree};
use crate::ui::{Open, Switcher, Ui};

/// The chord that opens the list and closes it again. Every other chord takes
/// the list away instead ([`super::layered`]).
pub(super) const CHORD: char = 'g';

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
        put_away(ui, now);
        ui.layer.close(now.instant);
        return Vec::new();
    }
    let rows = tree::roster(tree, &[]);
    // The box is the query from here on, so the line being written goes into
    // the gesture's own keeping and the list opens on an empty one.
    let draft = ui.composer.take();
    ui.layer.show(
        Open::Switcher(Switcher {
            cursor: roster::Cursor::on(&roster::listing(tree, &rows, ""), tree.view()),
            draft,
            stored: Vec::new(),
            // Where the walk started, so `esc` can put it back.
            from: Some(tree.view().clone()),
        }),
        now.instant,
    );
    ui.edited();
    vec![Effect::ListStored]
}

/// The list owns the keyboard while it is up: `↑`/`↓` walk the one column,
/// labels and all, and the view goes with the cursor — walking the list *is*
/// the switch, as the strip's walk was (§3). `tab` completes the name under
/// the cursor into the box, `⏎` settles on where the walk landed, `esc` takes
/// the query back and then the list, and every other key is the box's.
pub(super) fn keys(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    if let Some((cursor, chosen)) = walked(ui, tree, key) {
        return walk_to(ui, tree, cursor, chosen);
    }
    match key.code {
        KeyCode::Tab => completed(ui, tree),
        KeyCode::Enter | KeyCode::Esc => settle(ui, tree, key, now),
        _ => typed(ui, tree, key),
    }
}

/// `tab` puts the name the cursor is on into the box — a session's name, a
/// room's `#name` — the way `tab` on the `/` dropdown completes a command, and
/// the list narrows to what it left. The cursor stays on the row it completed:
/// a name a person can read is a name the matcher keeps.
fn completed(ui: &mut Ui, tree: &Tree) -> Vec<Effect> {
    let Open::Switcher(open) = &ui.layer.open else {
        return Vec::new();
    };
    let query = ui.composer.text();
    let was = open.session(tree, query, open.cursor);
    let Some(name) = open.name(tree, query, open.cursor) else {
        return Vec::new();
    };
    ui.composer.set(&name);
    ui.edited();
    replaced(ui, tree, was)
}

/// Where the arrows move the cursor, and the session that lands under it.
/// `None` says the key was not one of the list's own.
fn walked(ui: &Ui, tree: &Tree, key: KeyEvent) -> Option<(roster::Cursor, Option<SessionId>)> {
    let Open::Switcher(open) = &ui.layer.open else {
        return None;
    };
    let rows = tree::roster(tree, &open.stored);
    let listing = roster::listing(tree, &rows, ui.composer.text());
    let cursor = match key.code {
        KeyCode::Up => open.cursor.step(&listing, -1),
        KeyCode::Down => open.cursor.step(&listing, 1),
        _ => return None,
    };
    Some((cursor, cursor.row(&listing).map(|row| row.session.clone())))
}

/// Every key that is not the list's own is the box's: the query is a line like
/// any other, and the list is ranked by whatever the key leaves in it.
fn typed(ui: &mut Ui, tree: &Tree, key: KeyEvent) -> Vec<Effect> {
    let was = on_cursor(ui, tree);
    if !edits(ui, key) {
        return Vec::new();
    }
    ui.edited();
    replaced(ui, tree, was)
}

/// What a key does to the box, and whether it was the box's at all. A word
/// chord is spelled in one table only ([`super::alt`]); the control chords
/// never reach here, because [`super::layered`] answers them first.
fn edits(ui: &mut Ui, key: KeyEvent) -> bool {
    if key.modifiers.contains(KeyModifiers::ALT) {
        super::alt(ui, key);
        return true;
    }
    match key.code {
        KeyCode::Char(c) => ui.composer.insert(&c.to_string()),
        KeyCode::Backspace => ui.composer.backspace(),
        KeyCode::Delete => ui.composer.delete(),
        KeyCode::Left => ui.composer.left(),
        KeyCode::Right => ui.composer.right(),
        KeyCode::Home => ui.composer.home(),
        KeyCode::End => ui.composer.end(),
        _ => return false,
    }
    true
}

/// The session the cursor names on the list as the box leaves it.
fn on_cursor(ui: &Ui, tree: &Tree) -> Option<SessionId> {
    let Open::Switcher(open) = &ui.layer.open else {
        return None;
    };
    open.session(tree, ui.composer.text(), open.cursor)
}

/// The list once the box reads this. The cursor keeps the session it was on
/// where the query left that row on the list, and takes the first row there is
/// where it did not; the view follows the cursor as it does on a walk, so `⏎`
/// keeps what a person is looking at.
fn replaced(ui: &mut Ui, tree: &Tree, was: Option<SessionId>) -> Vec<Effect> {
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
    let listing = roster::listing(tree, &rows, ui.composer.text());
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
/// so settling is only putting the list away. `esc` is §7's stack: the query a
/// person typed is the first thing it takes back, and the list itself the
/// next, putting back the session it was opened from.
fn settle(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    let Open::Switcher(open) = &ui.layer.open else {
        return Vec::new();
    };
    let opened_from = open.from.clone();
    match key.code {
        KeyCode::Enter => put_away(ui, now),
        KeyCode::Esc if !ui.composer.is_empty() => return cleared(ui, tree),
        KeyCode::Esc => {
            put_away(ui, now);
            return opened_from
                .map(|id| vec![Effect::View(id)])
                .unwrap_or_default();
        }
        _ => {}
    }
    Vec::new()
}

/// The first `esc`: the query goes and the list stays, widened back to
/// everything with the cursor still on the session it was on.
fn cleared(ui: &mut Ui, tree: &Tree) -> Vec<Effect> {
    let was = on_cursor(ui, tree);
    ui.composer.clear();
    ui.edited();
    replaced(ui, tree, was)
}

/// Put the list away: the draft it set aside goes back in the box exactly as
/// it was, caret at its end, as a recalled line is. The one way out, so a
/// query a person typed is never left behind in the box for `⏎` to send.
pub(super) fn put_away(ui: &mut Ui, now: Now) {
    let Open::Switcher(open) = &ui.layer.open else {
        return;
    };
    // One already on its way out has given the draft back.
    if !ui.layer.showing() {
        return;
    }
    let draft = open.draft.clone();
    ui.composer.set(&draft);
    ui.edited();
    ui.layer.close(now.instant);
}
