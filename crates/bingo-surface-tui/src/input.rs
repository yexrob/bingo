//! One pure function from a key to a list of effects. It mutates the surface's
//! own `Ui` and reads the folded `SessionState`; it calls nothing, so a key
//! table is a test with no terminal and no kernel in it.
//!
//! Only lines that reach the kernel are appended to the history file: the loop
//! writes what it submits, and a surface-local command never gets that far.

use std::path::PathBuf;

use bingo_sdk::{Input, Level, Origin, SessionId, SessionSelector, SessionSpec, SessionState};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Position;

use crate::SURFACE_ID;
use crate::clock::Now;
use crate::commands::{self, Local};
use crate::complete;
use crate::effect::Effect;
use crate::keys;
use crate::pager;
use crate::rail::{self, CardId, Pin};
use crate::rewind::{self, Rewind};
use crate::search::Search;
use crate::select::Cell;
use crate::tree::Tree;
use crate::ui::{Open, Pending, Switcher, Ui};
use crate::{panel, permission, views};

/// What the first ctrl+c on an empty composer says.
pub const ARM_HINT: &str = "press ctrl+c again to exit";
/// What shift+tab says when no policy published a mode it can walk.
pub const UNKNOWN_MODE: &str = "permission mode unknown — /permission <mode>";
/// What ctrl+g says when the session has spawned nobody to switch to.
pub const NO_AGENTS: &str = "no sub-agents in this session";
/// Lines one notch of the wheel moves the transcript.
const WHEEL: isize = 3;

pub fn on_key(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    if key.kind == KeyEventKind::Release {
        return Vec::new();
    }
    ui.block = None;
    // `esc esc` needs no clock: any other key is what says the two were not
    // one gesture.
    ui.esc_armed &= key.code == KeyCode::Esc;
    let state = tree.viewed();
    if let Some(effects) = leaving(ui, state, key, now) {
        return effects;
    }
    if ui.select.run.is_some() {
        return selecting(ui, tree, key, now);
    }
    if ui.search.is_some() {
        return searching(ui, key, now);
    }
    if let Some(effects) = layered(ui, tree, key, now) {
        return effects;
    }
    if key.code == KeyCode::Esc {
        return escape(ui, tree, now);
    }
    if key.code == KeyCode::Tab && ui.suggestions(cwd(tree)).is_empty() && cycle_focus(ui, tree) {
        return Vec::new();
    }
    // A prompt raised anywhere in the tree is answered from wherever the
    // person is looking; the handle routes the answer back to who asked.
    if let Some((_, interaction)) = tree.open_interaction() {
        return ui.dialog.on_key(interaction, key, now);
    }
    if let Some(effects) = menu(ui, tree, key) {
        return effects;
    }
    editing(ui, tree, key, now)
}

/// What a layer answers, and the chords that open one.
///
/// A list and the pager own every key while they are up — the chords included,
/// so `g` is the pager's and not the switcher's for as long as a block is open.
/// The panel and the switcher leave the chords alone, because the chord that
/// opened one is what closes it.
fn layered(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Option<Vec<Effect>> {
    if ui.layer.captures() {
        match &ui.layer.open {
            Open::Picker(_) => return Some(picker(ui, key, now)),
            Open::Pager(_) => return Some(pager::keys(ui, tree, key, now)),
            Open::Rewind(_) => return Some(rewind_keys(ui, tree, key, now)),
            _ => {}
        }
    }
    if let Some(chord) = chorded(key) {
        match chord {
            'f' => ui.search = Some(Search::open()),
            'g' => toggle_switcher(ui, tree, now),
            't' => ui.layer.toggle(Open::Panel, now.instant),
            'o' => deepen(ui, tree, now),
            _ => return None,
        }
        return Some(Vec::new());
    }
    if !ui.layer.captures() {
        return None;
    }
    Some(match ui.layer.open {
        Open::Panel => panel_keys(ui, tree, key, now),
        _ => switcher(ui, tree, key, now),
    })
}

/// The letter of a control chord, if that is what this key is.
fn chorded(key: KeyEvent) -> Option<char> {
    match (key.code, key.modifiers.contains(KeyModifiers::CONTROL)) {
        (KeyCode::Char(c), true) => Some(c),
        _ => None,
    }
}

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
/// takes the focus and starts a run.
fn pressed(ui: &mut Ui, tree: &Tree, mouse: MouseEvent, now: Now) -> Vec<Effect> {
    if let Some(index) = card_option(ui, mouse) {
        return answer(ui, tree, index, now);
    }
    if let Some(card) = rail_card(ui, mouse) {
        ui.focus = Some(card);
        return Vec::new();
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
    ui.select.block = block;
    ui.select.start(cell);
    Vec::new()
}

/// A drag takes the far end of the run with it.
fn drag(ui: &mut Ui, mouse: MouseEvent) {
    if let Some(cell) = transcript_cell(ui, mouse) {
        ui.select.extend(cell);
    }
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
    let row = usize::from(mouse.row - region.y);
    // A short transcript hangs from the foot of its region: the rows above it
    // are padding and belong to no line.
    let padding = region.height as usize - painted.height.min(region.height as usize);
    Some(Cell {
        line: painted.top + row.checked_sub(padding)?,
        column: usize::from(mouse.column - region.x),
    })
}

/// `ctrl+o` only ever opens further: the first press lifts the fold on the
/// focused result — else the latest — and the second takes the whole of it into
/// the pager, where `esc` folds it again. One key, one direction (§4's
/// `ctrl+o to expand`).
fn deepen(ui: &mut Ui, tree: &Tree, now: Now) {
    let focused = ui.select.block.clone();
    let Some(id) = latest(tree.viewed(), focused.as_ref(), has_result) else {
        return;
    };
    if ui.expanded.contains(&id) {
        pager::open_block(ui, tree, now, Some(&id));
        return;
    }
    ui.expanded.insert(id);
}

/// The newest item a key acts on: the focused block when the transcript is
/// holding one, else the last that answers `what`.
pub fn latest(
    state: &SessionState,
    focused: Option<&bingo_sdk::ItemId>,
    what: impl Fn(&bingo_sdk::Item) -> bool,
) -> Option<bingo_sdk::ItemId> {
    state
        .items
        .iter()
        .rev()
        .filter(|item| focused.is_none_or(|id| id == &item.id))
        .find(|item| what(item))
        .map(|item| item.id.clone())
}

fn has_result(item: &bingo_sdk::Item) -> bool {
    match &item.body {
        bingo_sdk::ItemBody::ToolCall { output, .. } => output.is_some(),
        _ => false,
    }
}

/// The panel sheet answers its own keys: the cursor walks the kinds the
/// plugins published, and `⏎` pins one into the rail or takes it back.
fn panel_keys(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    let rows = panel::rows(tree.viewed());
    match key.code {
        KeyCode::Up => ui.panel = ui.panel.saturating_sub(1),
        KeyCode::Down => ui.panel = (ui.panel + 1).min(rows.len().saturating_sub(1)),
        KeyCode::Esc => ui.layer.close(now.instant),
        KeyCode::Enter => pin(ui, tree.view(), rows.get(ui.panel)),
        _ => {}
    }
    Vec::new()
}

fn pin(ui: &mut Ui, session: &SessionId, card: Option<&CardId>) {
    let Some(card) = card else {
        return;
    };
    let pin = Pin {
        session: session.clone(),
        card: card.clone(),
    };
    if !ui.pinned.remove(&pin) {
        ui.pinned.insert(pin);
    }
}

/// `tab` walks the rail's cards and then comes back round to none, so the
/// digits belong to what a person is typing again (design §7: focus moves by
/// opening and closing, never by an ambient event). A rail with no cards in
/// it leaves the key to whoever wanted it.
fn cycle_focus(ui: &mut Ui, tree: &Tree) -> bool {
    let cards = rail::cards(tree.viewed(), tree.view(), &ui.pinned);
    if cards.is_empty() {
        return false;
    }
    let at = ui
        .focus
        .as_ref()
        .and_then(|id| cards.iter().position(|card| &card.id == id));
    ui.focus = match at {
        Some(at) if at + 1 == cards.len() => None,
        Some(at) => Some(cards[at + 1].id.clone()),
        None => Some(cards[0].id.clone()),
    };
    true
}

/// A key on the focused card fires the action it names (ADR-0013 §3). The
/// button wears the mark until the session's stream moves, which is the ack.
fn fire(ui: &mut Ui, tree: &Tree, key: char) -> Option<Vec<Effect>> {
    let state = tree.viewed();
    let focus = ui.focus.clone()?;
    let cards = rail::cards(state, tree.view(), &ui.pinned);
    let card = cards.iter().find(|card| card.id == focus)?;
    let action = views::actions::fired(&views::actions_of(&card.body), key)?
        .action
        .clone();
    ui.pending = Some(Pending {
        action: action.clone(),
        seq: state.seq,
    });
    Some(vec![Effect::Submit(Input::Action { action })])
}

/// The directory a session's `@` mentions are read from.
fn cwd(tree: &Tree) -> &str {
    &tree.viewed().summary.cwd
}

/// A bracketed paste lands verbatim wherever the caret is.
pub fn on_paste(ui: &mut Ui, text: &str) {
    ui.composer.insert(text);
    ui.edited();
}

/// The two chords that can end the run, checked before anything may swallow
/// them: a dialog that ate ctrl+c would turn the interrupt into a letter.
fn leaving(ui: &mut Ui, state: &SessionState, key: KeyEvent, now: Now) -> Option<Vec<Effect>> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Char('c') => Some(interrupt_or_exit(ui, state, now)),
        KeyCode::Char('d') if ui.composer.is_empty() => Some(vec![Effect::Exit]),
        _ => None,
    }
}

/// What ctrl+c does is [`keys::interrupt`]'s table; this is only the doing of
/// it.
fn interrupt_or_exit(ui: &mut Ui, state: &SessionState, now: Now) -> Vec<Effect> {
    let pressed = keys::Pressed {
        busy: state.busy(),
        typing: !ui.composer.is_empty(),
        armed: ui.exit_armed(now.instant),
    };
    match keys::interrupt(pressed) {
        keys::Interrupt::Turn => return vec![Effect::Interrupt],
        keys::Interrupt::Clear => {
            ui.composer.clear();
            ui.edited();
        }
        keys::Interrupt::Exit => return vec![Effect::Exit],
        // The hint is derived from `armed` on the status line, never queued
        // behind a notice: the answer to a key outruns everything else.
        keys::Interrupt::Arm => ui.armed = Some(now.instant),
    }
    Vec::new()
}

/// Esc closes the innermost thing that is open and then interrupts, in the
/// order of [`keys::ESCAPES`]. Leaving a card is the card's own answer, so
/// that rung goes back to the dialog.
fn escape(ui: &mut Ui, tree: &Tree, now: Now) -> Vec<Effect> {
    let open = keys::Open {
        // One layer is open at a time, whichever form it takes.
        sheet: ui.layer.showing(),
        card: tree.open_interaction().is_some(),
        dropdown: !ui.suggestions(cwd(tree)).is_empty(),
        busy: tree.viewed().busy(),
    };
    // An `esc` that closed something is not half of a gesture.
    let rung = keys::escape(open);
    ui.esc_armed &= rung.is_none();
    match rung {
        Some(keys::Escape::Sheet) => ui.layer.close(now.instant),
        Some(keys::Escape::Card) => return cancel(ui, tree, now),
        Some(keys::Escape::Dropdown) => ui.menu.dismissed = true,
        Some(keys::Escape::Interrupt) => return vec![Effect::Interrupt],
        // The stack was empty, so this `esc` is one of `esc esc`.
        None => twice(ui, tree, now),
    }
    Vec::new()
}

/// `esc esc` on an empty composer opens the rewind picker (design §3). The
/// first press arms it; the second opens the card, when the session has a
/// `/rewind` for it to run.
fn twice(ui: &mut Ui, tree: &Tree, now: Now) {
    if !ui.composer.is_empty() {
        return;
    }
    if !std::mem::replace(&mut ui.esc_armed, true) {
        return;
    }
    ui.esc_armed = false;
    if !rewind::offered(&ui.commands()) {
        return;
    }
    let turns = rewind::turns(tree.viewed());
    if turns.is_empty() {
        return;
    }
    ui.layer.show(Open::Rewind(Rewind::default()), now.instant);
}

/// The rewind card answers its own keys, as every list does.
fn rewind_keys(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    let turns = rewind::turns(tree.viewed());
    let rows = rewind::rows(&turns);
    let Open::Rewind(card) = &mut ui.layer.open else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Up => card.selected = card.selected.saturating_sub(1),
        KeyCode::Down => card.selected = (card.selected + 1).min(rows.saturating_sub(1)),
        KeyCode::Esc => ui.layer.close(now.instant),
        KeyCode::Enter => {
            let chosen = turns.get(card.selected).map(rewind::line);
            ui.layer.close(now.instant);
            return chosen
                .map(|line| {
                    vec![Effect::Submit(Input::text(
                        line,
                        Origin::surface(SURFACE_ID),
                    ))]
                })
                .unwrap_or_default();
        }
        _ => {}
    }
    Vec::new()
}

/// Leaving the card that is asking: the kernel's own cancel or denial, which
/// the dialog knows and this does not.
fn cancel(ui: &mut Ui, tree: &Tree, now: Now) -> Vec<Effect> {
    let Some((_, interaction)) = tree.open_interaction() else {
        return Vec::new();
    };
    ui.dialog.on_key(
        interaction,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        now,
    )
}

fn picker(ui: &mut Ui, key: KeyEvent, now: Now) -> Vec<Effect> {
    let Open::Picker(picker) = &mut ui.layer.open else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
        KeyCode::Down => {
            picker.selected = (picker.selected + 1).min(picker.sessions.len().saturating_sub(1))
        }
        KeyCode::Char(c @ '1'..='9') => {
            picker.selected = (c as usize) - ('1' as usize);
        }
        KeyCode::Esc => ui.layer.close(now.instant),
        KeyCode::Enter => {
            let chosen = picker.sessions.get(picker.selected).map(|s| s.id.clone());
            ui.layer.close(now.instant);
            if let Some(id) = chosen {
                return vec![Effect::Open(SessionSelector::ById { id })];
            }
        }
        _ => {}
    }
    Vec::new()
}

/// A run owns the keyboard while it is being drawn: the arrows take its far
/// end, `y` and `ctrl+c` copy it, anything else lets it go and is typed.
fn selecting(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    let height = ui.transcript().0;
    match key.code {
        KeyCode::Up => ui.select.walk(-1, 0, height),
        KeyCode::Down => ui.select.walk(1, 0, height),
        KeyCode::Left => ui.select.walk(0, -1, height),
        KeyCode::Right => ui.select.walk(0, 1, height),
        KeyCode::Char('y') | KeyCode::Char('c') => return copy(ui),
        KeyCode::Esc => ui.select.clear(),
        _ => {
            ui.select.clear();
            return on_key(ui, tree, key, now);
        }
    }
    Vec::new()
}

/// Take what is inside the run, and let it go: a selection is answered once.
fn copy(ui: &mut Ui) -> Vec<Effect> {
    let text = ui
        .select
        .run
        .map(|run| run.text(&ui.transcript_text()))
        .unwrap_or_default();
    ui.select.clear();
    match text.is_empty() {
        true => Vec::new(),
        false => vec![Effect::Copy(text)],
    }
}

/// Whether the transcript, rather than the composer, is what the keys are
/// for: nothing is being typed, and the person has either scrolled back or
/// put the pointer on a block. `v` is a letter the rest of the time — a
/// message that starts with one is worth more than a chord that never waits.
fn reading(ui: &Ui) -> bool {
    ui.composer.is_empty() && (!ui.scroll.following() || ui.select.block.is_some())
}

/// Start a run at the top of the block the transcript is holding, or at the
/// first line on the screen when it is holding none.
fn start_selection(ui: &mut Ui) {
    let at = crate::select::Cell {
        line: focused_line(ui),
        column: 0,
    };
    ui.select.start(at);
}

fn focused_line(ui: &Ui) -> usize {
    let painted = ui.painted.borrow();
    ui.select
        .block
        .as_ref()
        .and_then(|item| painted.blocks.span(item).map(|(first, _)| first))
        .unwrap_or(painted.top)
}

/// The search row owns the keyboard while it is up: typing edits the query,
/// `enter` commits it and then steps, `n`/`N` walk the hits and `esc` gives
/// the status line back.
fn searching(ui: &mut Ui, key: KeyEvent, now: Now) -> Vec<Effect> {
    let Some(search) = ui.search.as_mut() else {
        return Vec::new();
    };
    match (search.typing, key.code) {
        (_, KeyCode::Esc) => ui.search = None,
        (true, KeyCode::Char(c)) => search.typed(c),
        (true, KeyCode::Backspace) => search.backspace(),
        (true, KeyCode::Enter) => commit(ui, now),
        (false, KeyCode::Char('n') | KeyCode::Enter) => step(ui, 1, now),
        (false, KeyCode::Char('N')) => step(ui, -1, now),
        _ => {}
    }
    Vec::new()
}

/// Look through what the last frame rendered — the blocks are the transcript
/// a person is reading — and go to the first hit.
fn commit(ui: &mut Ui, now: Now) {
    let lines = ui.transcript_text();
    if let Some(search) = ui.search.as_mut() {
        search.find(&lines);
    }
    step(ui, 0, now);
}

fn step(ui: &mut Ui, by: isize, now: Now) {
    let Some(search) = ui.search.as_mut() else {
        return;
    };
    search.step(by);
    let Some(hit) = search.current() else {
        return;
    };
    let (total, rows) = ui.transcript();
    ui.scroll.show(hit.line, total, rows, now.instant);
}

/// Open the switcher on the session in view, or close it again. There is
/// nothing to switch between until the session has spawned somebody.
fn toggle_switcher(ui: &mut Ui, tree: &Tree, now: Now) {
    if ui.layer.showing() {
        ui.layer.close(now.instant);
        return;
    }
    let rows = tree.rows();
    if rows.len() < 2 {
        ui.notify(Level::Info, NO_AGENTS, now.instant);
        return;
    }
    let selected = rows
        .iter()
        .position(|row| row.session == tree.view())
        .unwrap_or(0);
    ui.layer
        .show(Open::Switcher(Switcher { selected }), now.instant);
}

fn switcher(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    let rows = tree.rows();
    let Open::Switcher(switcher) = &mut ui.layer.open else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Up => switcher.selected = switcher.selected.saturating_sub(1),
        KeyCode::Down => {
            switcher.selected = (switcher.selected + 1).min(rows.len().saturating_sub(1))
        }
        KeyCode::Esc => ui.layer.close(now.instant),
        KeyCode::Enter => {
            let chosen = rows.get(switcher.selected).map(|row| row.session.clone());
            ui.layer.close(now.instant);
            return chosen.map(|id| vec![Effect::View(id)]).unwrap_or_default();
        }
        _ => {}
    }
    Vec::new()
}

/// The dropdown owns the arrows and the completion keys while it is open.
fn menu(ui: &mut Ui, tree: &Tree, key: KeyEvent) -> Option<Vec<Effect>> {
    let rows = ui.suggestions(cwd(tree));
    if rows.is_empty() {
        return (key.code == KeyCode::Tab).then(Vec::new);
    }
    match key.code {
        KeyCode::Up => ui.menu.selected = ui.menu.selected.saturating_sub(1),
        KeyCode::Down => ui.menu.selected = (ui.menu.selected + 1).min(rows.len() - 1),
        KeyCode::Tab => return Some(accept(ui, tree)),
        // Enter completes only while there is something left to complete; a
        // name already typed in full is meant to run.
        KeyCode::Enter if adds_something(ui, tree) => return Some(accept(ui, tree)),
        KeyCode::Enter => {
            ui.menu.dismissed = true;
            return None;
        }
        _ => return None,
    }
    Some(Vec::new())
}

fn adds_something(ui: &Ui, tree: &Tree) -> bool {
    ui.selected_suggestion(cwd(tree))
        .is_some_and(|chosen| chosen.value.trim_end() != ui.composer.text().trim_end())
}

fn accept(ui: &mut Ui, tree: &Tree) -> Vec<Effect> {
    if let Some(chosen) = ui.selected_suggestion(cwd(tree)) {
        ui.composer.set(&chosen.value);
    }
    ui.edited();
    Vec::new()
}

fn editing(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return control(ui, key);
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return alt(ui, key);
    }
    plain(ui, tree, key, now)
}

fn control(ui: &mut Ui, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Char('a') => ui.composer.home(),
        KeyCode::Char('e') => ui.composer.end(),
        KeyCode::Char('j') => newline(ui),
        KeyCode::Char('w') => edit(ui, |c| c.delete_word_left()),
        KeyCode::Char('u') => edit(ui, |c| c.delete_to_line_start()),
        KeyCode::Char('k') => edit(ui, |c| c.delete_to_line_end()),
        _ => {}
    }
    Vec::new()
}

fn alt(ui: &mut Ui, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Char('b') => ui.composer.word_left(),
        KeyCode::Char('f') => ui.composer.word_right(),
        KeyCode::Enter => newline(ui),
        _ => {}
    }
    Vec::new()
}

fn plain(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    match key.code {
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => newline(ui),
        KeyCode::Enter => return enter(ui, tree, now),
        KeyCode::BackTab => return cycle_mode(ui, tree.viewed(), now),
        KeyCode::Up => history_or_line(ui, Step::Up),
        KeyCode::Down => history_or_line(ui, Step::Down),
        KeyCode::PageUp => scroll(ui, ui.page() as isize, now),
        KeyCode::PageDown => scroll(ui, -(ui.page() as isize), now),
        KeyCode::Left => ui.composer.left(),
        KeyCode::Right => ui.composer.right(),
        KeyCode::Home if ui.composer.is_empty() => {
            let (total, rows) = ui.transcript();
            ui.scroll.home(total, rows, now.instant)
        }
        KeyCode::End if ui.composer.is_empty() => ui.scroll.end(),
        KeyCode::Home => ui.composer.home(),
        KeyCode::End => ui.composer.end(),
        KeyCode::Backspace => edit(ui, |c| c.backspace()),
        KeyCode::Delete => edit(ui, |c| c.delete()),
        KeyCode::Char('?') if ui.composer.is_empty() => ui.layer.toggle(Open::Help, now.instant),
        KeyCode::Char('v') if reading(ui) => start_selection(ui),
        // A key belongs to the focused card's buttons while one is focused and
        // nothing is being typed; the rest of the time it is a letter.
        KeyCode::Char(c) if ui.composer.is_empty() && ui.focus.is_some() => {
            if let Some(effects) = fire(ui, tree, c) {
                return effects;
            }
            edit(ui, |composer| composer.insert(&c.to_string()));
        }
        KeyCode::Char(c) => edit(ui, |composer| composer.insert(&c.to_string())),
        _ => {}
    }
    Vec::new()
}

/// Shift+tab asks the policy for the next mode, as a typed `/permission`
/// would: the kernel decides, and the badge moves when the config frame lands.
/// Nothing is assumed here, so a refused command leaves the screen truthful.
fn cycle_mode(ui: &mut Ui, state: &SessionState, now: Now) -> Vec<Effect> {
    let Some(next) = permission::next(state) else {
        ui.notify(Level::Warn, UNKNOWN_MODE, now.instant);
        return Vec::new();
    };
    vec![Effect::Submit(Input::text(
        format!("/permission {next}"),
        Origin::surface(SURFACE_ID),
    ))]
}

/// Enter sends, unless the line ends in a backslash — the newline chord for
/// terminals that cannot tell shift+enter from enter.
fn enter(ui: &mut Ui, tree: &Tree, now: Now) -> Vec<Effect> {
    // A block the transcript is holding is what `⏎` is for while nothing is
    // being typed: it opens whole (design §5). A message outranks it.
    if ui.composer.is_empty()
        && let Some(block) = ui.select.block.clone()
        && pager::open_block(ui, tree, now, Some(&block))
    {
        return Vec::new();
    }
    if ui.composer.text().ends_with('\\') {
        ui.composer.backspace();
        newline(ui);
        return Vec::new();
    }
    if ui.composer.text().trim().is_empty() {
        return Vec::new();
    }
    submit(ui, tree, now)
}

/// What a line does. `/clear` starts a fresh session beside the root's, not
/// beside whichever child is on screen.
fn submit(ui: &mut Ui, tree: &Tree, now: Now) -> Vec<Effect> {
    let text = ui.composer.take();
    ui.history.remember(&text);
    ui.edited();
    ui.scroll.end();
    match commands::local(&text) {
        Some(Local::Help) => {
            ui.layer.toggle(Open::Help, now.instant);
            Vec::new()
        }
        Some(Local::Clear) => vec![Effect::Open(SessionSelector::Create {
            spec: SessionSpec {
                cwd: PathBuf::from(&tree.root().summary.cwd),
                ..SessionSpec::default()
            },
        })],
        Some(Local::Resume(Some(id))) => vec![Effect::Open(SessionSelector::ById {
            id: bingo_sdk::SessionId::from_raw(id),
        })],
        Some(Local::Resume(None)) => vec![Effect::ListSessions],
        Some(Local::Exit) => vec![Effect::Exit],
        // A picture a person mentioned reaches the model as a part beside the
        // line, and the line still says which word it was.
        None => vec![Effect::Submit(Input::Text {
            attachments: complete::attachments(&text),
            text,
            origin: Origin::surface(SURFACE_ID),
        })],
    }
}

enum Step {
    Up,
    Down,
}

/// The arrows walk the buffer until it runs out, then the prompt history.
fn history_or_line(ui: &mut Ui, step: Step) {
    let moved = match step {
        Step::Up => ui.composer.up(),
        Step::Down => ui.composer.down(),
    };
    if moved {
        return;
    }
    let recalled = match step {
        Step::Up => ui.history.older(ui.composer.text()),
        Step::Down => ui.history.newer(),
    };
    if let Some(text) = recalled {
        ui.composer.set(&text);
        ui.menu = Default::default();
    }
}

/// Move the transcript, against the frame the last draw measured.
pub fn scroll(ui: &mut Ui, lines: isize, now: Now) {
    let (total, rows) = ui.transcript();
    ui.scroll.by(lines, total, rows, now.instant);
}

fn newline(ui: &mut Ui) {
    edit(ui, |composer| composer.newline());
}

fn edit(ui: &mut Ui, change: impl FnOnce(&mut crate::composer::Composer)) {
    change(&mut ui.composer);
    ui.edited();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use bingo_sdk::{Activation, Answer, SessionId, TurnStatus};
    use crossterm::event::KeyCode;

    /// A session whose own directory has two files a mention could name.
    fn with_files() -> (tempfile::TempDir, SessionState) {
        let dir = tempfile::tempdir().expect("a directory");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").expect("a manifest");
        std::fs::write(dir.path().join("shot.png"), "png").expect("a picture");
        let mut state = state();
        state.summary.cwd = dir.path().to_string_lossy().into_owned();
        (dir, state)
    }

    #[test]
    fn an_at_sign_offers_the_paths_under_the_session_and_enter_takes_one() {
        let (_dir, state) = with_files();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        write(&mut ui, &state, "@Car", now);
        assert_eq!(
            ui.suggestions(&state.summary.cwd)
                .iter()
                .map(|row| row.label.clone())
                .collect::<Vec<_>>(),
            vec!["@Cargo.toml".to_string()],
        );
        on_key(&mut ui, &tree, key(KeyCode::Enter), now);
        assert_eq!(ui.composer.text(), "@Cargo.toml ");
        assert!(
            ui.suggestions(&state.summary.cwd).is_empty(),
            "a finished mention offers nothing more"
        );
    }

    #[test]
    fn a_mentioned_picture_travels_beside_the_line() {
        let (_dir, state) = with_files();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        write(&mut ui, &state, "look at @shot.png", now);
        let effects = on_key(&mut ui, &tree, key(KeyCode::Enter), now);
        assert_eq!(
            effects,
            vec![Effect::Submit(Input::Text {
                text: "look at @shot.png".into(),
                attachments: vec!["shot.png".into()],
                origin: Origin::surface(SURFACE_ID),
            })],
        );
    }

    /// A transcript of two turns, and a session that can rewind to one.
    fn rewindable() -> (SessionState, Vec<bingo_sdk::CommandSpec>) {
        let mut state = state();
        state.items = vec![
            in_turn(
                "itm_1",
                "trn_1",
                user("itm_1", "what is in this workspace?"),
            ),
            in_turn("itm_2", "trn_2", user("itm_2", "write me a note")),
        ];
        (
            state,
            vec![bingo_sdk::CommandSpec {
                name: "rewind".into(),
                aliases: Vec::new(),
                hint: "go back to a turn".into(),
                args: bingo_sdk::ArgSpec::Free {
                    hint: "<turn>".into(),
                },
                instant: true,
                family: "session".into(),
            }],
        )
    }

    fn in_turn(id: &str, turn: &str, mut item: bingo_sdk::Item) -> bingo_sdk::Item {
        item.id = bingo_sdk::ItemId::from_raw(id);
        item.turn = Some(bingo_sdk::TurnId::from_raw(turn));
        item
    }

    #[test]
    fn esc_twice_on_an_empty_composer_lists_the_turns_and_enter_rewinds() {
        let (state, commands) = rewindable();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        ui.catalogs.commands = commands;
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(ui.esc_armed, "the first one arms it and closes nothing");
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(ui.layer.is(&Open::Rewind(Rewind::default())));

        on_key(&mut ui, &tree, key(KeyCode::Down), now);
        let effects = on_key(&mut ui, &tree, key(KeyCode::Enter), now);
        assert_eq!(
            effects,
            vec![Effect::Submit(Input::text(
                "/rewind trn_1",
                Origin::surface(SURFACE_ID),
            ))],
            "the second row is the older turn"
        );
    }

    #[test]
    fn a_key_between_the_two_escapes_is_what_says_they_were_not_one_gesture() {
        let (state, commands) = rewindable();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        ui.catalogs.commands = commands;
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        on_key(&mut ui, &tree, typed('a'), now);
        assert!(!ui.esc_armed);
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(!ui.layer.showing(), "and a half-typed line is not empty");
    }

    /// The picker is offered only where the session has a `/rewind` to run;
    /// as of M11e nothing registers one.
    #[test]
    fn nothing_opens_where_the_session_cannot_rewind() {
        let (state, _) = rewindable();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(!ui.layer.showing());
        assert!(ui.notices.is_empty(), "and it is silent about it");
    }

    /// The row the switcher's cursor is on, when it is the layer that is open.
    fn selected(ui: &Ui) -> Option<usize> {
        match &ui.layer.open {
            Open::Switcher(switcher) => Some(switcher.selected),
            _ => None,
        }
    }

    fn press(
        ui: &mut Ui,
        state: &SessionState,
        key: crossterm::event::KeyEvent,
        now: Now,
    ) -> Vec<Effect> {
        on_key(ui, &solo(state), key, now)
    }

    fn press_tree(
        ui: &mut Ui,
        tree: &Tree,
        key: crossterm::event::KeyEvent,
        now: Now,
    ) -> Vec<Effect> {
        on_key(ui, tree, key, now)
    }

    /// A root with one sub-agent under it.
    fn with_child(mut frames: Vec<bingo_sdk::Frame>) -> Tree {
        frames.insert(0, child_frame(1, announced("reviewer")));
        folded_tree(frames)
    }

    fn line(ui: &mut Ui, state: &SessionState, text: &str, now: Now) -> Vec<Effect> {
        write(ui, state, text, now);
        press(ui, state, key(KeyCode::Enter), now)
    }

    fn busy() -> SessionState {
        folded(vec![frame(1, started("trn_1"))])
    }

    // ---- submitting -----------------------------------------------------

    #[test]
    fn enter_on_an_empty_composer_does_nothing() {
        let (mut ui, now) = scene();
        assert!(press(&mut ui, &state(), key(KeyCode::Enter), now).is_empty());
        write(&mut ui, &state(), "   ", now);
        assert!(press(&mut ui, &state(), key(KeyCode::Enter), now).is_empty());
    }

    #[test]
    fn prose_and_the_kernels_own_commands_are_submitted_verbatim() {
        for text in ["hello there", "/model fake/fake-2", "!ls -la"] {
            let (mut ui, now) = scene();
            assert_eq!(
                line(&mut ui, &state(), text, now),
                vec![Effect::Submit(Input::text(text, Origin::surface("tui")))],
                "{text}"
            );
        }
    }

    #[test]
    fn clear_opens_a_new_session_in_the_same_directory() {
        let (mut ui, now) = scene();
        assert_eq!(
            line(&mut ui, &state(), "/clear", now),
            vec![Effect::Open(SessionSelector::Create {
                spec: SessionSpec {
                    cwd: PathBuf::from("/tmp/project"),
                    ..SessionSpec::default()
                }
            })]
        );
    }

    #[test]
    fn exit_and_quit_leave_and_resume_opens_or_asks() {
        let (mut ui, now) = scene();
        assert_eq!(line(&mut ui, &state(), "/exit", now), vec![Effect::Exit]);
        assert_eq!(line(&mut ui, &state(), "/quit", now), vec![Effect::Exit]);
        assert_eq!(
            line(&mut ui, &state(), "/resume ses_9", now),
            vec![Effect::Open(SessionSelector::ById {
                id: SessionId::from_raw("ses_9")
            })]
        );
        assert_eq!(
            line(&mut ui, &state(), "/resume", now),
            vec![Effect::ListSessions]
        );
    }

    #[test]
    fn help_is_the_surfaces_own_and_reaches_nobody() {
        let (mut ui, now) = scene();
        assert!(line(&mut ui, &state(), "/help", now).is_empty());
        assert!(ui.layer.is(&Open::Help));
    }

    // ---- the permission mode --------------------------------------------

    #[test]
    fn shift_tab_asks_for_the_next_mode_and_wraps() {
        let cycle = [
            ("default", "acceptEdits"),
            ("acceptEdits", "plan"),
            ("plan", "bypassPermissions"),
            ("bypassPermissions", "dontAsk"),
            ("dontAsk", "default"),
        ];
        for (mode, next) in cycle {
            let (mut ui, now) = scene();
            assert_eq!(
                press(
                    &mut ui,
                    &with_permission_mode(mode),
                    shift(KeyCode::BackTab),
                    now
                ),
                vec![Effect::Submit(Input::text(
                    format!("/permission {next}"),
                    Origin::surface("tui")
                ))],
                "{mode}"
            );
            assert!(ui.notices.is_empty(), "{mode}");
        }
    }

    #[test]
    fn shift_tab_says_so_when_the_mode_is_not_one_it_knows() {
        for state in [state(), with_permission_mode("acceptedits")] {
            let (mut ui, now) = scene();
            assert!(press(&mut ui, &state, shift(KeyCode::BackTab), now).is_empty());
            assert!(ui.notices.iter().any(|n| n.text == UNKNOWN_MODE));
        }
    }

    #[test]
    fn shift_tab_leaves_the_draft_where_it_was() {
        let state = with_permission_mode("default");
        let (mut ui, now) = scene();
        write(&mut ui, &state, "half a thought", now);
        press(&mut ui, &state, shift(KeyCode::BackTab), now);
        assert_eq!(ui.composer.text(), "half a thought");
    }

    #[test]
    fn an_open_dialog_keeps_shift_tab() {
        let state = folded(vec![
            frame(1, permission_view("default")),
            frame(2, opened(permission(Some("Edit(src/)"), None))),
        ]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        assert!(
            press(&mut ui, &state, shift(KeyCode::BackTab), now).is_empty(),
            "the dialog answers keys, and it has no answer for this one"
        );
    }

    // ---- newlines -------------------------------------------------------

    #[test]
    fn every_newline_chord_inserts_one() {
        for key in [
            crossterm::event::KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::SHIFT),
            ctrl('j'),
            crate::test_support::alt(KeyCode::Enter),
        ] {
            let (mut ui, now) = scene();
            write(&mut ui, &state(), "one", now);
            assert!(press(&mut ui, &state(), key, now).is_empty());
            write(&mut ui, &state(), "two", now);
            assert_eq!(ui.composer.text(), "one\ntwo");
        }
    }

    #[test]
    fn a_trailing_backslash_turns_enter_into_a_newline() {
        let (mut ui, now) = scene();
        write(&mut ui, &state(), "one\\", now);
        assert!(press(&mut ui, &state(), key(KeyCode::Enter), now).is_empty());
        assert_eq!(ui.composer.text(), "one\n");
    }

    // ---- leaving --------------------------------------------------------

    #[test]
    fn ctrl_c_interrupts_then_clears_then_arms_then_exits() {
        let (mut ui, now) = scene();
        assert_eq!(
            press(&mut ui, &busy(), ctrl('c'), now),
            vec![Effect::Interrupt]
        );

        write(&mut ui, &state(), "draft", now);
        assert!(press(&mut ui, &state(), ctrl('c'), now).is_empty());
        assert!(ui.composer.is_empty(), "text goes first");

        assert!(press(&mut ui, &state(), ctrl('c'), now).is_empty());
        let line = crate::status::line(&solo(&state()), &ui, 80, now).to_string();
        assert!(line.contains(ARM_HINT), "{line}");
        let late = Now {
            instant: now.instant + crate::ui::EXIT_WINDOW,
            ..now
        };
        assert!(
            press(&mut ui, &state(), ctrl('c'), late).is_empty(),
            "the arm lapses"
        );
        assert_eq!(press(&mut ui, &state(), ctrl('c'), now), vec![Effect::Exit]);
    }

    #[test]
    fn ctrl_d_leaves_only_on_an_empty_composer() {
        let (mut ui, now) = scene();
        assert_eq!(press(&mut ui, &state(), ctrl('d'), now), vec![Effect::Exit]);
        write(&mut ui, &state(), "x", now);
        assert!(press(&mut ui, &state(), ctrl('d'), now).is_empty());
    }

    // ---- esc ------------------------------------------------------------

    /// One press per rung of [`keys::ESCAPES`], in the order the stack is
    /// obeyed: a sheet over a card is what `esc` closes first, because
    /// dismissing the help must never answer the question underneath it.
    #[test]
    fn esc_closes_the_innermost_thing_then_interrupts() {
        let state = folded(vec![
            frame(1, started("trn_1")),
            frame(2, opened(permission(None, None))),
        ]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        ui.layer.show(Open::Help, now.instant);

        assert!(press(&mut ui, &state, key(KeyCode::Esc), now).is_empty());
        assert!(!ui.layer.showing(), "the sheet is the outermost thing");

        assert_eq!(
            press(&mut ui, &state, key(KeyCode::Esc), now),
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::Deny { feedback: None },
                activation: Activation::Keyboard,
            }],
            "then the card, whose own answer leaving it is"
        );

        let busy = busy();
        write(&mut ui, &busy, "/he", now);
        assert!(press(&mut ui, &busy, key(KeyCode::Esc), now).is_empty());
        assert!(ui.menu.dismissed, "then the dropdown");

        assert_eq!(
            press(&mut ui, &busy, key(KeyCode::Esc), now),
            vec![Effect::Interrupt],
            "then the running turn"
        );
    }

    // ---- the dialogs ----------------------------------------------------

    #[test]
    fn the_session_option_answers_with_the_scope_the_kernel_named() {
        let state = folded(vec![frame(1, opened(permission(Some("Edit(src/)"), None)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        assert_eq!(
            press(&mut ui, &state, typed('2'), now),
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::AllowSession {
                    scope: "Edit(src/)".into()
                },
                activation: Activation::Keyboard,
            }]
        );
        assert!(
            press(&mut ui, &state, typed('1'), now).is_empty(),
            "an answered dialog sends nothing more"
        );
    }

    #[test]
    fn refusing_collects_the_words_that_go_with_it() {
        let state = folded(vec![frame(1, opened(permission(None, None)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        assert!(press(&mut ui, &state, typed('n'), now).is_empty());
        write(&mut ui, &state, "edit the test instead", now);
        assert_eq!(
            press(&mut ui, &state, key(KeyCode::Enter), now),
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::Deny {
                    feedback: Some("edit the test instead".into())
                },
                activation: Activation::Keyboard,
            }]
        );
    }

    #[test]
    fn an_empty_refusal_carries_no_feedback() {
        let state = folded(vec![frame(1, opened(permission(None, None)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        press(&mut ui, &state, typed('n'), now);
        assert_eq!(
            press(&mut ui, &state, key(KeyCode::Enter), now),
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::Deny { feedback: None },
                activation: Activation::Keyboard,
            }]
        );
    }

    #[test]
    fn a_key_before_the_guard_sends_nothing() {
        let mut interaction = permission(Some("Edit(src/)"), None);
        interaction.guard_until = Some(ts() + jiff::SignedDuration::from_secs(1));
        let state = folded(vec![frame(1, opened(interaction))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        assert!(press(&mut ui, &state, typed('1'), now).is_empty());
        let after = Now {
            wall: ts() + jiff::SignedDuration::from_secs(2),
            ..now
        };
        assert_eq!(press(&mut ui, &state, typed('1'), after).len(), 1);
    }

    #[test]
    fn y_and_n_name_the_answers_wherever_they_sit() {
        let state = folded(vec![frame(1, opened(permission(Some("Edit(src/)"), None)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        assert_eq!(
            press(&mut ui, &state, typed('y'), now),
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::AllowOnce,
                activation: Activation::Keyboard,
            }]
        );
    }

    #[test]
    fn a_single_choice_question_answers_on_the_first_key() {
        let state = folded(vec![frame(1, opened(question(false, false)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        assert_eq!(
            press(&mut ui, &state, key(KeyCode::Down), now),
            Vec::new(),
            "arrows only move"
        );
        assert_eq!(
            press(&mut ui, &state, key(KeyCode::Enter), now),
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::Choice {
                    ids: vec!["o".into()]
                },
                activation: Activation::Keyboard,
            }]
        );
    }

    #[test]
    fn a_multiple_choice_question_toggles_then_confirms() {
        let state = folded(vec![frame(1, opened(question(true, false)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        press(&mut ui, &state, typed(' '), now);
        press(&mut ui, &state, key(KeyCode::Down), now);
        press(&mut ui, &state, typed(' '), now);
        press(&mut ui, &state, typed(' '), now);
        assert_eq!(
            press(&mut ui, &state, key(KeyCode::Enter), now),
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::Choice {
                    ids: vec!["a".into()]
                },
                activation: Activation::Keyboard,
            }],
            "the second option was toggled on and off again"
        );
    }

    #[test]
    fn a_question_that_offers_cancel_takes_esc_as_one() {
        let state = folded(vec![frame(1, opened(question(false, false)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        assert_eq!(
            press(&mut ui, &state, key(KeyCode::Esc), now),
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::Cancel,
                activation: Activation::Keyboard,
            }]
        );
    }

    #[test]
    fn a_free_text_question_sends_the_words_as_text() {
        let state = folded(vec![frame(1, opened(question(false, true)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        press(&mut ui, &state, typed('3'), now);
        write(&mut ui, &state, "neither", now);
        assert_eq!(
            press(&mut ui, &state, key(KeyCode::Enter), now),
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::Text {
                    text: "neither".into()
                },
                activation: Activation::Keyboard,
            }]
        );
    }

    #[test]
    fn the_dialog_state_resets_when_the_next_interaction_opens() {
        let mut state = folded(vec![frame(1, opened(permission(None, None)))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        press(&mut ui, &state, typed('n'), now);
        assert!(ui.dialog.words.is_some());
        state.apply(&frame(2, resolved()));
        let mut next = question(false, false);
        next.id = bingo_sdk::InteractionId::from_raw("int_2");
        state.apply(&frame(3, opened(next)));
        ui.dialog.focus_on(state.interactions.first());
        assert!(ui.dialog.words.is_none());
        assert!(!ui.dialog.answered);
    }

    // ---- the dropdown and the history ------------------------------------

    #[test]
    fn tab_completes_and_enter_runs_what_is_already_whole() {
        let (mut ui, now) = scene();
        write(&mut ui, &state(), "/cle", now);
        press(&mut ui, &state(), key(KeyCode::Tab), now);
        assert_eq!(ui.composer.text(), "/clear ");

        let (mut ui, now) = scene();
        write(&mut ui, &state(), "/clear", now);
        assert_eq!(
            press(&mut ui, &state(), key(KeyCode::Enter), now).len(),
            1,
            "a name that is already whole runs instead of completing"
        );
    }

    #[test]
    fn the_arrows_walk_the_prompt_history_at_the_edges() {
        let (mut ui, now) = scene();
        line(&mut ui, &state(), "first", now);
        line(&mut ui, &state(), "second", now);
        press(&mut ui, &state(), key(KeyCode::Up), now);
        assert_eq!(ui.composer.text(), "second");
        press(&mut ui, &state(), key(KeyCode::Up), now);
        assert_eq!(ui.composer.text(), "first");
        press(&mut ui, &state(), key(KeyCode::Down), now);
        assert_eq!(ui.composer.text(), "second");
        press(&mut ui, &state(), key(KeyCode::Down), now);
        assert_eq!(ui.composer.text(), "", "and back to the draft");
    }

    #[test]
    fn the_arrows_move_inside_a_multi_line_draft_first() {
        let (mut ui, now) = scene();
        line(&mut ui, &state(), "history", now);
        write(&mut ui, &state(), "one", now);
        press(&mut ui, &state(), ctrl('j'), now);
        write(&mut ui, &state(), "two", now);
        press(&mut ui, &state(), key(KeyCode::Up), now);
        assert_eq!(ui.composer.text(), "one\ntwo", "the caret only moved");
        press(&mut ui, &state(), key(KeyCode::Up), now);
        assert_eq!(ui.composer.text(), "history");
    }

    #[test]
    fn a_paste_lands_verbatim() {
        let (mut ui, _) = scene();
        on_paste(&mut ui, "two\nlines");
        assert_eq!(ui.composer.text(), "two\nlines");
    }

    #[test]
    fn the_question_mark_only_opens_the_panel_on_an_empty_composer() {
        let (mut ui, now) = scene();
        press(&mut ui, &state(), typed('?'), now);
        assert!(ui.layer.is(&Open::Help));
        press(&mut ui, &state(), typed('?'), now);
        assert!(!ui.layer.is(&Open::Help));
        write(&mut ui, &state(), "why", now);
        press(&mut ui, &state(), typed('?'), now);
        assert_eq!(ui.composer.text(), "why?");
        assert!(!ui.layer.is(&Open::Help));
    }

    #[test]
    fn a_release_event_is_not_a_press() {
        let (mut ui, now) = scene();
        let mut key = typed('x');
        key.kind = crossterm::event::KeyEventKind::Release;
        press(&mut ui, &state(), key, now);
        assert!(ui.composer.is_empty());
    }

    #[test]
    fn the_picker_opens_the_session_it_lands_on() {
        let (mut ui, now) = scene();
        ui.layer.show(
            Open::Picker(crate::ui::Picker {
                sessions: vec![
                    summary(),
                    bingo_sdk::SessionSummary {
                        id: SessionId::from_raw("ses_2"),
                        ..summary()
                    },
                ],
                selected: 0,
            }),
            now.instant,
        );
        press(&mut ui, &state(), key(KeyCode::Down), now);
        assert_eq!(
            press(&mut ui, &state(), key(KeyCode::Enter), now),
            vec![Effect::Open(SessionSelector::ById {
                id: SessionId::from_raw("ses_2")
            })]
        );
        assert!(!ui.layer.showing());
    }

    // ---- the switcher ---------------------------------------------------

    #[test]
    fn ctrl_g_lists_the_tree_and_enter_switches_the_view() {
        let tree = with_child(vec![]);
        let (mut ui, now) = scene();
        assert!(press_tree(&mut ui, &tree, ctrl('g'), now).is_empty());
        assert_eq!(selected(&ui), Some(0), "it opens on the session in view");
        press_tree(&mut ui, &tree, key(KeyCode::Down), now);
        assert_eq!(
            press_tree(&mut ui, &tree, key(KeyCode::Enter), now),
            vec![Effect::View(child_id())]
        );
        assert!(!ui.layer.showing());
    }

    #[test]
    fn ctrl_g_toggles_and_esc_closes_the_switcher() {
        let tree = with_child(vec![]);
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        press_tree(&mut ui, &tree, ctrl('g'), now);
        assert!(!ui.layer.showing(), "the same chord closes it");
        press_tree(&mut ui, &tree, ctrl('g'), now);
        press_tree(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(!ui.layer.showing());
    }

    #[test]
    fn ctrl_g_says_so_when_there_is_nobody_to_switch_to() {
        let (mut ui, now) = scene();
        assert!(press(&mut ui, &state(), ctrl('g'), now).is_empty());
        assert!(!ui.layer.showing());
        assert!(ui.notices.iter().any(|n| n.text == NO_AGENTS));
    }

    #[test]
    fn the_switcher_opens_on_the_child_that_is_already_in_view() {
        let mut tree = with_child(vec![]);
        tree.show(&child_id());
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        assert_eq!(selected(&ui), Some(1));
        assert_eq!(
            press_tree(&mut ui, &tree, key(KeyCode::Enter), now),
            vec![Effect::View(child_id())]
        );
    }

    #[test]
    fn a_prompt_a_child_raised_is_answered_from_the_root_view() {
        let tree = with_child(vec![child_frame(2, opened(child_permission()))]);
        let (mut ui, now) = scene();
        ui.dialog
            .focus_on(tree.open_interaction().map(|(_, open)| open));
        assert_eq!(
            press_tree(&mut ui, &tree, typed('y'), now),
            vec![Effect::Answer {
                interaction: bingo_sdk::InteractionId::from_raw("int_2"),
                answer: Answer::AllowOnce,
                activation: Activation::Keyboard,
            }],
            "the root's handle routes it back to whoever asked"
        );
    }

    #[test]
    fn clear_starts_beside_the_root_even_from_a_child_view() {
        let mut tree = with_child(vec![]);
        tree.show(&child_id());
        let (mut ui, now) = scene();
        write(&mut ui, tree.viewed(), "/clear", now);
        assert_eq!(
            press_tree(&mut ui, &tree, key(KeyCode::Enter), now),
            vec![Effect::Open(SessionSelector::Create {
                spec: SessionSpec {
                    cwd: PathBuf::from("/tmp/project"),
                    ..SessionSpec::default()
                }
            })]
        );
    }

    // ---- selection and the clipboard ------------------------------------

    #[test]
    fn v_starts_a_run_the_arrows_extend_and_y_copies_it() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        press(&mut ui, &state, key(KeyCode::PageUp), now);
        render(&state, &ui, now);
        press(&mut ui, &state, typed('v'), now);
        assert!(ui.select.run.is_some(), "v starts one while reading back");

        press(&mut ui, &state, key(KeyCode::Down), now);
        press(&mut ui, &state, key(KeyCode::Right), now);
        let copied = press(&mut ui, &state, typed('y'), now);
        let [Effect::Copy(text)] = copied.as_slice() else {
            panic!("y copies: {copied:?}")
        };
        assert_eq!(
            text.lines().count(),
            2,
            "a run of two lines, cut where the far end is: {text:?}"
        );
        assert!(ui.select.run.is_none(), "copying lets it go");
    }

    #[test]
    fn a_letter_ends_a_run_and_is_typed() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        press(&mut ui, &state, key(KeyCode::PageUp), now);
        render(&state, &ui, now);
        press(&mut ui, &state, typed('v'), now);
        press(&mut ui, &state, typed('h'), now);
        assert!(ui.select.run.is_none());
        assert_eq!(ui.composer.text(), "h");
    }

    #[test]
    fn v_is_a_letter_while_the_transcript_is_at_its_foot() {
        let (mut ui, now) = scene();
        write(&mut ui, &state(), "ver", now);
        assert!(ui.select.run.is_none(), "a message may start with one");
        assert_eq!(ui.composer.text(), "ver");
    }

    #[test]
    fn esc_lets_a_run_go() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        press(&mut ui, &state, key(KeyCode::PageUp), now);
        render(&state, &ui, now);
        press(&mut ui, &state, typed('v'), now);
        assert!(press(&mut ui, &state, key(KeyCode::Esc), now).is_empty());
        assert!(ui.select.run.is_none());
    }

    // ---- the mouse ------------------------------------------------------

    #[test]
    fn the_wheel_scrolls_the_transcript_and_the_foot_takes_it_back() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        on_mouse(&mut ui, &solo(&state), wheel(true, 10, 5), now);
        let (total, rows) = ui.transcript();
        assert_eq!(
            ui.scroll
                .top(total, rows, now.instant + crate::scroll::EASE),
            total - rows - WHEEL as usize
        );
        for _ in 0..3 {
            on_mouse(&mut ui, &solo(&state), wheel(false, 10, 5), now);
        }
        assert_eq!(ui.scroll, crate::scroll::Scroll::Tail);
    }

    #[test]
    fn a_click_in_the_transcript_focuses_the_block_it_landed_on() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        on_mouse(&mut ui, &solo(&state), click(4, 19), now);
        assert_eq!(
            ui.select.block,
            Some(bingo_sdk::ItemId::from_raw("itm_59")),
            "the last row is the last item"
        );
        assert!(ui.select.run.is_some(), "and a run starts there");
    }

    #[test]
    fn a_drag_takes_the_far_end_of_the_run_with_it() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        on_mouse(&mut ui, &solo(&state), click(2, 17), now);
        on_mouse(&mut ui, &solo(&state), dragged(6, 19), now);
        let run = ui.select.run.expect("a run");
        assert_eq!(run.anchor.column, 2);
        assert_eq!(run.head.column, 6);
        assert_eq!(run.head.line, run.anchor.line + 2);
    }

    #[test]
    fn a_click_on_a_child_row_steps_into_it() {
        let tree = folded_tree(vec![
            frame(
                1,
                bingo_sdk::Event::ItemCompleted {
                    item: tool(
                        "itm_1",
                        "SpawnAgent",
                        serde_json::json!({"prompt": "review it"}),
                        None,
                        bingo_sdk::ItemStatus::Completed,
                    ),
                },
            ),
            child_frame(1, announced("reviewer")),
        ]);
        let (mut ui, now) = scene();
        render_tree(&tree, &ui, now);
        let row = ui.painted.borrow().regions.transcript.bottom() - 1;
        assert_eq!(
            on_mouse(&mut ui, &tree, click(4, row), now),
            vec![Effect::View(child_id())],
            "the `↳` row belongs to the call that spawned it"
        );
        assert!(ui.select.run.is_none(), "stepping in is not selecting");
    }

    #[test]
    fn a_click_on_a_card_row_answers_it() {
        let state = folded(vec![frame(1, opened(permission(Some("Edit(src/)"), None)))]);
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        render(&state, &ui, now);
        let card = ui.painted.borrow().card.clone().expect("a card on screen");
        let row = card
            .options
            .iter()
            .position(|option| option == &Some(1))
            .expect("the second option has a row");
        let effects = on_mouse(
            &mut ui,
            &solo(&state),
            click(4, card.area.y + 1 + row as u16),
            now,
        );
        assert_eq!(
            effects,
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::AllowSession {
                    scope: "Edit(src/)".into()
                },
                activation: Activation::Keyboard,
            }]
        );
    }

    // ---- the plugin-state panel -----------------------------------------

    #[test]
    fn ctrl_t_toggles_the_plugin_state_panel_and_esc_closes_it() {
        let (mut ui, now) = scene();
        assert!(press(&mut ui, &state(), ctrl('t'), now).is_empty());
        assert!(ui.layer.is(&Open::Panel));
        press(&mut ui, &state(), ctrl('t'), now);
        assert!(!ui.layer.is(&Open::Panel), "the same chord closes it");

        press(&mut ui, &state(), ctrl('t'), now);
        assert!(press(&mut ui, &state(), key(KeyCode::Esc), now).is_empty());
        assert!(!ui.layer.showing(), "and so does esc");
    }

    #[test]
    fn one_layer_at_a_time_replaces_the_last() {
        let (mut ui, now) = scene();
        press(&mut ui, &state(), typed('?'), now);
        assert!(ui.layer.is(&Open::Help));
        press(&mut ui, &state(), ctrl('t'), now);
        assert!(ui.layer.is(&Open::Panel), "focus moves, never sideways");
    }

    #[test]
    fn what_is_closing_stays_on_screen_until_it_has_gone() {
        let (mut ui, now) = scene();
        press(&mut ui, &state(), typed('?'), now);
        press(&mut ui, &state(), key(KeyCode::Esc), now);
        assert!(!ui.layer.showing(), "it is on its way out");
        assert!(
            !ui.layer.reveal(now).gone(),
            "and still drawn while it goes"
        );
        ui.expire(frames_at(now, 4));
        assert_eq!(ui.layer.open, Open::Nothing);
    }

    #[test]
    fn a_failed_turn_does_not_change_what_a_key_does() {
        let state = folded(vec![frame(
            1,
            completed(
                "trn_1",
                TurnStatus::Failed {
                    error: bingo_sdk::KernelError::new(bingo_sdk::ErrorCode::Internal, "boom"),
                },
            ),
        )]);
        let (mut ui, now) = scene();
        assert_eq!(
            line(&mut ui, &state, "again", now),
            vec![Effect::Submit(Input::text("again", Origin::surface("tui")))]
        );
    }

    #[test]
    fn a_view_block_is_dismissed_by_the_next_key() {
        let (mut ui, now) = scene();
        ui.block = Some(bingo_sdk::View::Text { text: "x".into() });
        press(&mut ui, &state(), key(KeyCode::Left), now);
        assert!(ui.block.is_none());
    }

    #[test]
    fn the_page_keys_scroll_a_screenful_and_come_back_to_the_tail() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        let (total, rows) = ui.transcript();
        assert!(total > rows, "a transcript worth scrolling");

        press(&mut ui, &state, key(KeyCode::PageUp), now);
        let settled = Now {
            instant: now.instant + crate::scroll::EASE,
            ..now
        };
        assert_eq!(
            ui.scroll.top(total, rows, settled.instant),
            total - rows - rows,
            "a page is the screenful being read"
        );
        assert_ne!(
            ui.scroll,
            crate::scroll::Scroll::Tail,
            "pgup releases the tail"
        );

        press(&mut ui, &state, key(KeyCode::PageDown), settled);
        assert_eq!(
            ui.scroll,
            crate::scroll::Scroll::Tail,
            "and pgdn at the foot takes it back"
        );
    }

    #[test]
    fn home_and_end_walk_the_transcript_while_nothing_is_typed() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        let (total, rows) = ui.transcript();
        press(&mut ui, &state, key(KeyCode::Home), now);
        assert_eq!(
            ui.scroll
                .top(total, rows, now.instant + crate::scroll::EASE),
            0
        );
        press(&mut ui, &state, key(KeyCode::End), now);
        assert_eq!(ui.scroll, crate::scroll::Scroll::Tail);

        write(&mut ui, &state, "a line", now);
        press(&mut ui, &state, key(KeyCode::Home), now);
        assert_eq!(
            ui.scroll,
            crate::scroll::Scroll::Tail,
            "a draft keeps home for the caret"
        );
    }

    #[test]
    fn the_kill_chords_cut_the_buffer() {
        let (mut ui, now) = scene();
        write(&mut ui, &state(), "alpha beta", now);
        press(&mut ui, &state(), ctrl('w'), now);
        assert_eq!(ui.composer.text(), "alpha ");
        press(&mut ui, &state(), ctrl('u'), now);
        assert_eq!(ui.composer.text(), "");
        write(&mut ui, &state(), "gamma", now);
        press(&mut ui, &state(), ctrl('a'), now);
        press(&mut ui, &state(), ctrl('k'), now);
        assert_eq!(ui.composer.text(), "");
    }

    #[test]
    fn submitting_clears_the_scroll_so_the_answer_is_visible() {
        let (mut ui, now) = scene();
        press(&mut ui, &state(), key(KeyCode::PageUp), now);
        line(&mut ui, &state(), "hello", now);
        assert_eq!(ui.scroll, crate::scroll::Scroll::Tail);
    }

    #[test]
    fn a_notice_frame_is_not_a_key_concern() {
        // Folding a notice never touches the composer: it is the loop's job.
        let mut state = state();
        state.apply(&frame(1, notice(bingo_sdk::Level::Warn, "estimating")));
        let (mut ui, now) = scene();
        write(&mut ui, &state, "x", now);
        assert_eq!(ui.composer.text(), "x");
    }
    // ---- the rail, its focus and its actions (ADR-0013 §3) --------------

    /// The action a press fired, when it fired one.
    fn action_of(effects: &[Effect]) -> Option<&bingo_sdk::Action> {
        effects.iter().find_map(|effect| match effect {
            Effect::Submit(Input::Action { action }) => Some(action),
            _ => None,
        })
    }

    #[test]
    fn tab_walks_the_rail_cards_and_comes_back_round_to_none() {
        let state = boarded();
        let (mut ui, now) = scene();
        pin_board(&mut ui);
        assert_eq!(ui.focus, None);
        press(&mut ui, &state, key(KeyCode::Tab), now);
        assert_eq!(ui.focus, Some(demo_card("board")));
        press(&mut ui, &state, key(KeyCode::Tab), now);
        assert_eq!(ui.focus, Some(demo_card("progress")));
        press(&mut ui, &state, key(KeyCode::Tab), now);
        assert_eq!(ui.focus, None, "and the keys are the composer's again");
    }

    #[test]
    fn tab_is_left_alone_when_the_rail_has_no_cards_to_walk() {
        let (mut ui, now) = scene();
        press(&mut ui, &state(), key(KeyCode::Tab), now);
        assert_eq!(ui.focus, None);
    }

    #[test]
    fn a_key_on_the_focused_card_fires_the_action_it_names() {
        let state = boarded();
        let (mut ui, now) = scene();
        pin_board(&mut ui);
        ui.focus = Some(demo_card("board"));
        let fired = press(&mut ui, &state, typed('1'), now);
        assert_eq!(
            action_of(&fired).map(|action| action.name.as_str()),
            Some("board.tick")
        );
        assert_eq!(ui.composer.text(), "", "the key was not typed as well");
        assert_eq!(
            ui.pending.as_ref().map(|pending| pending.seq),
            Some(state.seq),
            "the mark waits for the stream to move"
        );
    }

    #[test]
    fn a_key_the_focused_card_does_not_offer_is_a_letter_like_any_other() {
        let state = boarded();
        let (mut ui, now) = scene();
        pin_board(&mut ui);
        ui.focus = Some(demo_card("board"));
        assert!(press(&mut ui, &state, typed('9'), now).is_empty());
        assert_eq!(ui.composer.text(), "9");
    }

    #[test]
    fn a_digit_with_no_card_focused_is_typed() {
        let state = boarded();
        let (mut ui, now) = scene();
        pin_board(&mut ui);
        assert!(press(&mut ui, &state, typed('1'), now).is_empty());
        assert_eq!(ui.composer.text(), "1");
    }

    #[test]
    fn a_click_on_the_rail_focuses_the_card_it_landed_on() {
        let state = boarded();
        let (mut ui, now) = scene();
        pin_board(&mut ui);
        // The rail exists only once it has been drawn: a click is answered
        // against the frame the last draw left behind.
        draw_sized(120, 40, &state, &ui, now);
        let rail = ui
            .painted
            .borrow()
            .regions
            .rail
            .expect("a rail at 120 columns");
        let effects = on_mouse(&mut ui, &solo(&state), click(rail.x + 2, rail.y), now);
        assert!(effects.is_empty());
        assert_eq!(ui.focus, Some(demo_card("board")));
    }

    #[test]
    fn enter_in_the_panel_sheet_pins_a_panel_and_again_takes_it_back() {
        let state = boarded();
        let (mut ui, now) = scene();
        press(&mut ui, &state, ctrl('t'), now);
        assert!(ui.layer.captures(), "the sheet answers its own keys");
        press(&mut ui, &state, key(KeyCode::Enter), now);
        assert!(ui.pinned.contains(&crate::rail::Pin {
            session: bingo_sdk::SessionId::from_raw("ses_1"),
            card: demo_card("board"),
        }));
        press(&mut ui, &state, key(KeyCode::Enter), now);
        assert!(ui.pinned.is_empty(), "the same key takes it back");
        press(&mut ui, &state, ctrl('t'), now);
        assert!(!ui.layer.showing(), "ctrl+t still closes it");
    }
}
