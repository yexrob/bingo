//! One pure function from a key to a list of effects. It mutates the surface's
//! own `Ui` and reads the folded `SessionState`; it calls nothing, so a key
//! table is a test with no terminal and no kernel in it.
//!
//! Only lines that reach the kernel are appended to the history file: the loop
//! writes what it submits, and a surface-local command never gets that far.

use std::path::PathBuf;

use bingo_sdk::{Delivery, Input, IntentId, Level, SessionSelector, SessionSpec, SessionState};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::SURFACE_ID;
use crate::clock::Now;
use crate::commands::{self, Local};
use crate::effect::Effect;
use crate::fold;
use crate::keys;
use crate::mentions;
use crate::pager;
use crate::rewind::{self, Rewind};
use crate::search::Search;
use crate::tree::Tree;
use crate::ui::{Menu, Open, Ui};
use crate::views;

mod interaction;
mod mode;

pub(crate) use interaction::{cancel, interaction_keys, interaction_unhandled};

/// What the first ctrl+c on an empty composer says.
/// What shift+tab says when no policy published a mode it can walk.
pub const UNKNOWN_MODE: &str = "permission mode unknown — /permission <mode>";
/// What ctrl+b says when no shell command is running to background.
pub const NOTHING_RUNNING: &str = "no shell command is running";

pub fn on_key(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    if key.kind == KeyEventKind::Release {
        return Vec::new();
    }
    ui.prepare(&tree.viewed().summary.cwd);
    if key == KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL) && ui.composer.is_empty() {
        return vec![Effect::Exit];
    }
    if crate::sessions::active(ui) {
        return crate::sessions::key(ui, tree, key, now);
    }
    if crate::browse::active(ui) {
        return crate::browse::key(ui, tree, key, now);
    }
    if crate::plugins::active(ui) {
        return crate::plugins::key(ui, key, now);
    }
    if crate::palette::active(ui) {
        return crate::palette::key(ui, tree, key, now);
    }
    if ui.focus.showing() && matches!(ui.focus.open(), Open::Pager(_)) {
        return pager::keys(ui, tree, key, now);
    }
    if key.code == KeyCode::Esc && tree.open_interaction().is_some() {
        return cancel(ui, tree);
    }
    if key == KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL) && ui.dialog.aside {
        ui.dialog.aside = false;
        return Vec::new();
    }
    let owner = crate::focus::owner(ui, tree);
    if owner == crate::focus::Owner::Intro {
        ui.focus = crate::focus::Focus::Composer;
        return Vec::new();
    }
    ui.block = None;
    ui.esc_armed &= key.code == KeyCode::Esc;
    if let Some(effects) = leaving(ui, tree.viewed(), key, now) {
        return effects;
    }
    owned_keys(ui, tree, key, now, owner)
}

fn owned_keys(
    ui: &mut Ui,
    tree: &Tree,
    key: KeyEvent,
    now: Now,
    owner: crate::focus::Owner,
) -> Vec<Effect> {
    if owner == crate::focus::Owner::Pane && matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
        let backwards = key.code == KeyCode::BackTab;
        if !crate::pane::tab_control(ui, tree.viewed(), backwards) {
            cycle_focus(ui, tree, now, backwards);
        }
        return Vec::new();
    }
    if owner == crate::focus::Owner::Pane
        && let Some(action) = crate::pane::key_handler(ui, tree.viewed(), key)
    {
        return vec![Effect::SiteKey { action, key }];
    }
    match owner {
        crate::focus::Owner::Pane => pane_keys(ui, tree, key, now),
        crate::focus::Owner::Select => selecting(ui, tree, key, now),
        crate::focus::Owner::Search => searching(ui, key, now),
        crate::focus::Owner::Layer => {
            layered(ui, tree, key, now).unwrap_or_else(|| composer_keys(ui, tree, key, now))
        }
        crate::focus::Owner::Interaction => interaction_keys(ui, tree, key, now),
        _ => composer_keys(ui, tree, key, now),
    }
}

pub(crate) fn pane_keys(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    let effects = crate::pane::key(ui, tree.viewed(), key);
    if key.code != KeyCode::Esc && matches!(ui.focus, crate::focus::Focus::Composer) {
        return composer_keys(ui, tree, key, now);
    }
    effects
}

fn composer_keys(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    if matches!(key.code, KeyCode::Char(_)) && (key.modifiers - KeyModifiers::SHIFT).is_empty() {
        ui.last_typed = Some(now.wall);
    }
    if key == KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)
        && !crate::browse::offered(ui, tree)
    {
        ui.focus = crate::focus::Focus::Search(Search::open());
        return Vec::new();
    }
    if let Some(line) = keys::commands::line(ui, tree.viewed(), key) {
        return command_line(ui, tree, now, line);
    }
    if key == KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)
        && let Some(effects) = crate::palette::command(ui, "/resume", now)
    {
        return effects;
    }
    if let Some(effects) = layered(ui, tree, key, now) {
        return effects;
    }
    if key.code == KeyCode::Esc {
        return escape(ui, tree, now);
    }
    if key.code == KeyCode::BackTab && !crate::pane::focus_keys(ui, tree.viewed()).is_empty() {
        cycle_focus(ui, tree, now, true);
        return Vec::new();
    }
    if key.code == KeyCode::Tab {
        return tabbed(ui, tree, now);
    }
    if let Some(effects) = menu(ui, tree, key) {
        return effects;
    }
    editing(ui, tree, key, now)
}

/// `tab`, by the one table there is of it ([`keys::tab`]). Everything that
/// captures the keyboard has already answered by the time this runs, so the
/// rungs here are the dropdown, the running turn and the card ring.
fn tabbed(ui: &mut Ui, tree: &Tree, now: Now) -> Vec<Effect> {
    let at = keys::Tabbed {
        dropdown: !suggestions(ui, tree).is_empty(),
        busy: tree.viewed().busy(),
        typing: !ui.composer.text().trim().is_empty(),
        focusable: !row_actions(tree.viewed()).is_empty()
            || !crate::pane::focus_keys(ui, tree.viewed()).is_empty(),
    };
    match keys::tab(at) {
        Some(keys::Tab::Complete) => accept(ui, tree),
        // The same road out as `⏎`, with the one word that differs: this line
        // waits for the running turn rather than steering it.
        Some(keys::Tab::Queue) => submit(ui, tree, now, Delivery::Hold),
        Some(keys::Tab::Focus) => {
            cycle_focus(ui, tree, now, false);
            Vec::new()
        }
        None => Vec::new(),
    }
}

/// What a layer answers, and the chords that open one.
///
/// A list and the pager own every key while they are up — the layer chords
/// included, so `g` is the pager's and not the switcher's for as long as a
/// block is open. Help and the switcher leave those chords alone, because
/// the chord that opened one is what closes it; a chord that edits a line
/// ([`LAYERS`] does not name it) belongs to whoever is being typed into.
fn layered(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Option<Vec<Effect>> {
    if ui.focus.captures() {
        match ui.focus.open() {
            Open::Pager(_) => return Some(pager::keys(ui, tree, key, now)),
            Open::Rewind(_) => return Some(rewind_keys(ui, tree, key, now)),
            _ => {}
        }
    }
    if let Some(chord) = chorded(key) {
        match chord {
            'f' => ui.focus = crate::focus::Focus::Search(Search::open()),

            'o' => deepen(ui, tree, now),
            'b' => return Some(background(ui, tree, now)),
            // No other chord reaches here: `LAYERS` is what `chorded` answers.
            _ => return None,
        }
        return Some(Vec::new());
    }
    if !ui.focus.captures() {
        return None;
    }
    None
}

/// The control chords that open a layer of their own — the one table there is
/// of them. Every other control chord is the line's ([`control`]): `ctrl+w`
/// deletes a word wherever a person is typing, the list's own query included,
/// so it must not be mistaken for a layer's key.
const LAYERS: [char; 3] = ['f', 'o', 'b'];

/// The letter of a layer's control chord, if that is what this key is.
fn chorded(key: KeyEvent) -> Option<char> {
    match (key.code, key.modifiers.contains(KeyModifiers::CONTROL)) {
        (KeyCode::Char(c), true) if LAYERS.contains(&c) => Some(c),
        _ => None,
    }
}

/// `ctrl+o` only ever opens further: the first press lifts the fold on the
/// focused result — else the latest — and the second takes the whole of it into
/// the pager, where `esc` folds it again. One key, one direction (§4's
/// `ctrl+o to expand`).
fn deepen(ui: &mut Ui, tree: &Tree, now: Now) {
    let focused = ui.select.block.clone();
    let state = tree.viewed();
    // A thought a newer one joined has no block of its own (M79), so there is
    // nothing on the screen for the key to open: the run's rows are the last
    // thought's, and so is the fold under them.
    let opens = |item: &bingo_sdk::Item| folds(item) && crate::thoughts::ends_its_run(state, item);
    let Some(id) = latest(state, focused.as_ref(), opens) else {
        return;
    };
    let Some(item) = item_of(state, &id) else {
        return;
    };
    match fold::deeper(fold::fold_of(&ui.folds, item)) {
        Some(next) => {
            ui.folds.insert(id, next);
        }
        None => {
            pager::open_block(ui, tree, now, Some(&id));
        }
    }
}

/// `ctrl+b` hands the shell command that is running to the background: the
/// plugin's own command, fired by name with the call it is to act on
/// (ADR-0018 §6). The plugin does the rest — the call returns early with a job
/// id, and its row follows the signal it already publishes.
fn background(ui: &mut Ui, tree: &Tree, now: Now) -> Vec<Effect> {
    let Some(press) = placed_action(tree.viewed(), 'b') else {
        ui.notify(Level::Info, NOTHING_RUNNING, now.instant);
        return Vec::new();
    };
    vec![Effect::Submit(press)]
}

/// The newest unfinished item owns a key only when its producer places it,
/// and the press is signed as that producer's drawing.
fn placed_action(state: &SessionState, key: char) -> Option<bingo_sdk::Input> {
    let slots = bingo_sdk::slot::placed(state);
    state
        .transcript
        .items()
        .iter()
        .rev()
        .filter(|item| !item.is_terminal())
        .find_map(|item| {
            slots.iter().rev().find_map(|placed| match &placed.slot {
                bingo_sdk::slot::Slot::Row {
                    item: id, actions, ..
                } if id == &item.id => actions
                    .iter()
                    .find(|action| action.key == Some(key))
                    .map(|action| crate::pressed(&placed.plugin, action.action.clone())),
                _ => None,
            })
        })
}

/// The newest item a key acts on: the focused block when the transcript is
/// holding one, else the last that answers `what`.
pub fn latest(
    state: &SessionState,
    focused: Option<&bingo_sdk::ItemId>,
    what: impl Fn(&bingo_sdk::Item) -> bool,
) -> Option<bingo_sdk::ItemId> {
    state
        .transcript
        .items()
        .iter()
        .rev()
        .filter(|item| focused.is_none_or(|id| id == &item.id))
        .find(|item| what(item))
        .map(|item| item.id.clone())
}

/// The item a fold belongs to. A fold is a fact about a kind of block, so the
/// id alone cannot answer what the block starts at or cycles through.
pub(crate) fn item_of<'a>(
    state: &'a SessionState,
    id: &bingo_sdk::ItemId,
) -> Option<&'a bingo_sdk::Item> {
    state.transcript.get(id)
}

/// What `ctrl+o` opens further: a block that is keeping something back — a
/// call that came back, a thought that was not redacted, an action's own
/// result. Most of them say so on the row, with `… +N lines (ctrl+o to
/// expand)`; a thought that is over holds the rows it ended on and says
/// nothing, because the mark would cost it the row it is holding
/// (2026-09-06). A quiet notice is deliberately not one either: its cut
/// promises no key (M11's rule, 2026-09-01), and a click is what opens it.
fn folds(item: &bingo_sdk::Item) -> bool {
    match &item.body {
        bingo_sdk::ItemBody::ToolCall { output, .. } => output.is_some(),
        // Only once the thinking is over, as a call folds only once it has
        // come back: while it is being had the rows under the row are the tail
        // a running tool wears, which cuts nothing and promises no key.
        bingo_sdk::ItemBody::Reasoning { .. } => {
            item.completed_at.is_some() && crate::transcript::thought(item).is_some()
        }
        bingo_sdk::ItemBody::Action { result, .. } => result.is_some(),
        _ => false,
    }
}

/// Rows with actions, in transcript order, using the last placement as drawing does.
/// Each focusable row with the plugin that placed its actions.
type RowActions = (bingo_sdk::ItemId, String, Vec<bingo_sdk::ActionItem>);

fn row_actions(state: &SessionState) -> Vec<RowActions> {
    let placed = bingo_sdk::slot::placed(state);
    state
        .transcript
        .items()
        .iter()
        .filter_map(|item| {
            let (plugin, actions) = placed.iter().rev().find_map(|placed| match &placed.slot {
                bingo_sdk::slot::Slot::Row {
                    item: id, actions, ..
                } if id == &item.id => Some((&placed.plugin, actions)),
                _ => None,
            })?;
            (!actions.is_empty()).then(|| (item.id.clone(), plugin.clone(), actions.clone()))
        })
        .collect()
}

/// Panes and transcript rows share a ring with the composer.
fn cycle_focus(ui: &mut Ui, tree: &Tree, now: Now, backwards: bool) {
    use crate::focus::Focus;
    let mut ring = vec![Focus::Composer];
    ring.extend(
        crate::pane::focus_keys(ui, tree.viewed())
            .into_iter()
            .map(Focus::Pane),
    );
    ring.extend(
        row_actions(tree.viewed())
            .into_iter()
            .map(|(item, ..)| Focus::Row(item)),
    );
    let current = ring
        .iter()
        .position(|focus| match (focus, &ui.focus) {
            (Focus::Pane(a), Focus::Pane(b)) => a == b,
            (Focus::Row(a), Focus::Row(b)) => a == b,
            (Focus::Composer, Focus::Composer) => true,
            _ => false,
        })
        .unwrap_or_default();
    let next = if backwards {
        (current + ring.len() - 1) % ring.len()
    } else {
        (current + 1) % ring.len()
    };
    ui.focus = ring.swap_remove(next);
    if let Focus::Row(item) = &ui.focus
        && let Some((line, _)) = ui.shown.blocks.span(item)
    {
        ui.scroll.reveal(
            line,
            ui.shown.height,
            usize::from(ui.shown.regions.transcript.height),
            now.instant,
        );
    }
}

/// A focused row owns only the keys its actions offer.
fn fire(ui: &mut Ui, tree: &Tree, key: char) -> Option<Vec<Effect>> {
    let state = tree.viewed();
    let focus = ui.focus.row()?;
    let rows = row_actions(state);
    let (_, plugin, actions) = rows.iter().find(|(item, ..)| item == focus)?;
    let action = views::actions::fired(&actions.iter().collect::<Vec<_>>(), key)?
        .action
        .clone();
    Some(vec![Effect::Submit(crate::pressed(plugin, action))])
}

/// The directory a session's `@` mentions are read from.
fn cwd(tree: &Tree) -> &str {
    &tree.viewed().summary.cwd
}

/// What the dropdown is offering for the line being typed: the one question
/// every key that the dropdown owns asks, over the session on the screen.
fn suggestions(ui: &Ui, tree: &Tree) -> Vec<commands::Suggestion> {
    ui.suggestions(cwd(tree), &mentions::targets(tree))
}

/// A bracketed paste lands verbatim wherever the caret is.
pub fn on_paste(ui: &mut Ui, text: &str) {
    if ui.dialog.current().is_some() && !ui.dialog.aside {
        if let Some(words) = &mut ui.dialog.words {
            words.insert(text);
        }
        return;
    }
    if matches!(ui.focus, crate::focus::Focus::Pane(_)) {
        return;
    }
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
        keys::Interrupt::Turn => return ask_to_stop(ui, state),
        keys::Interrupt::Clear => {
            ui.composer.clear();
            ui.edited();
        }
        keys::Interrupt::Exit => return vec![Effect::Exit],
        keys::Interrupt::Arm => {
            ui.armed = Some(now.instant);
            ui.notice = None;
            ui.placed.toasts.discard();
        }
    }
    Vec::new()
}

/// Esc closes the innermost thing that is open and then interrupts, in the
/// order of [`keys::ESCAPES`]. Leaving a card is the card's own answer, so
/// that rung goes back to the dialog.
fn escape(ui: &mut Ui, tree: &Tree, now: Now) -> Vec<Effect> {
    if ui.focus.row().is_some() {
        ui.focus = crate::focus::Focus::Composer;
    }
    let open = keys::Open {
        // One layer is open at a time, whichever form it takes.
        sheet: ui.focus.showing(),
        card: tree.open_interaction().is_some(),
        dropdown: !suggestions(ui, tree).is_empty(),
        busy: tree.viewed().busy(),
        subsession: !tree.is_root(tree.view()) && ui.composer.is_empty(),
    };
    // An `esc` that closed something is not half of a gesture.
    let rung = keys::escape(open);
    ui.esc_armed &= rung.is_none();
    match rung {
        Some(keys::Escape::Sheet) => ui.focus.close(now.instant),
        Some(keys::Escape::Card) => return cancel(ui, tree),
        Some(keys::Escape::Dropdown) => ui.menu.dismissed = true,
        Some(keys::Escape::Interrupt) => return ask_to_stop(ui, tree.viewed()),
        Some(keys::Escape::Back) => {
            let parent = tree
                .viewed()
                .summary
                .parent
                .as_ref()
                .map_or_else(|| tree.root_id().clone(), |parent| parent.session.clone());
            return vec![Effect::View(parent)];
        }
        // The stack was empty, so this `esc` is one of `esc esc`.
        None => twice(ui, tree, now),
    }
    Vec::new()
}

/// Stop the turn, and remember on this frame that it was asked to.
///
/// The kernel decides what an interrupt does and its `TurnCompleted` ends the
/// story; but the actor's mailbox is first in, first out and may be mid-await,
/// so a row that waited for the kernel to answer would read as a dropped key
/// (§7). This is the same answer-the-key-now rule the armed `ctrl+c` hint
/// already follows — a fact about the keypress, not a copy of session state.
fn ask_to_stop(ui: &mut Ui, state: &SessionState) -> Vec<Effect> {
    ui.stop_asked = state.turn.as_ref().map(|turn| turn.id.clone());
    vec![Effect::Interrupt]
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
    let turns = bingo_sdk::transcript::turns(&tree.viewed().transcript);
    if turns.is_empty() {
        return;
    }
    ui.focus.show(Open::Rewind(Rewind::default()), now.instant);
}

/// The rewind card answers its own keys, as every list does.
fn rewind_keys(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    let turns = bingo_sdk::transcript::turns(&tree.viewed().transcript);
    let rows = rewind::rows(&turns);
    let Some(Open::Rewind(card)) = ui.focus.open_mut() else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Up => card.selected = card.selected.saturating_sub(1),
        KeyCode::Down => card.selected = (card.selected + 1).min(rows.saturating_sub(1)),
        KeyCode::Esc => ui.focus.close(now.instant),
        KeyCode::Enter => {
            let selected = turns.iter().rev().nth(card.selected).map(|t| t.id.clone());
            let chosen = turns.iter().rev().nth(card.selected).map(rewind::line);
            if crate::browse::offered(ui, tree) {
                if tree.viewed().busy() {
                    ui.notify(Level::Warn, crate::browse::BUSY, now.instant);
                }
                crate::browse::open(ui, tree, selected, true, now);
                return Vec::new();
            }
            ui.focus.close(now.instant);
            return chosen
                .map(|line| vec![Effect::Submit(Input::text(line, crate::origin()))])
                .unwrap_or_default();
        }
        _ => {}
    }
    Vec::new()
}

/// A run owns the keyboard while it is being drawn: the arrows take its far
/// end, `y` and `ctrl+c` copy it, anything else lets it go and is typed.
fn selecting(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    match key.code {
        KeyCode::Up => walk_run(ui, -1, 0, now),
        KeyCode::Down => walk_run(ui, 1, 0, now),
        KeyCode::Left => walk_run(ui, 0, -1, now),
        KeyCode::Right => walk_run(ui, 0, 1, now),
        KeyCode::Char('y') | KeyCode::Char('c') => return copy(ui),
        KeyCode::Esc => ui.clear_selection(),
        _ => {
            ui.clear_selection();
            return on_key(ui, tree, key, now);
        }
    }
    Vec::new()
}

/// Take the far end of the run where the key points, and bring the view after
/// it. A run's cells are lines of the whole transcript rather than of the
/// screenful it is parked on (§3), so the arrows reach what has scrolled away
/// — and what a person is choosing has to stay in front of them.
fn walk_run(ui: &mut Ui, lines: isize, columns: isize, now: Now) {
    let (height, rows) = ui.transcript();
    ui.walk_selection(lines, columns, height);
    let Some(head) = ui.focus.selection().map(|run| run.head) else {
        return;
    };
    ui.scroll.reveal(head.line, height, rows, now.instant);
}

/// Take what is inside the run, and let it go: a selection is answered once.
/// One road, whether a key asked or a button was released ([`crate::pointer`]).
pub fn copy(ui: &mut Ui) -> Vec<Effect> {
    let text = ui
        .focus
        .selection()
        .map(|run| run.text(&ui.transcript_text()))
        .unwrap_or_default();
    ui.clear_selection();
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
    ui.start_selection(at);
}

fn focused_line(ui: &Ui) -> usize {
    let painted = &ui.shown;
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
    let Some(search) = ui.focus.search_mut() else {
        return Vec::new();
    };
    match (search.typing, key.code) {
        (_, KeyCode::Esc) => ui.focus = crate::focus::Focus::Composer,
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
    if let Some(search) = ui.focus.search_mut() {
        search.find(&lines);
    }
    step(ui, 0, now);
}

fn step(ui: &mut Ui, by: isize, now: Now) {
    let Some(search) = ui.focus.search_mut() else {
        return;
    };
    search.step(by);
    let Some(hit) = search.current() else {
        return;
    };
    let (total, rows) = ui.transcript();
    ui.scroll.show(hit.line, total, rows, now.instant);
}

/// The dropdown owns the arrows and the enter key while it is open; `tab` is
/// answered before this, by the one table of it ([`tabbed`]).
fn menu(ui: &mut Ui, tree: &Tree, key: KeyEvent) -> Option<Vec<Effect>> {
    let rows = suggestions(ui, tree);
    if rows.is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Up => ui.menu.selected = ui.menu.selected.saturating_sub(1),
        KeyCode::Down => ui.menu.selected = (ui.menu.selected + 1).min(rows.len() - 1),
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
    chosen(ui, tree).is_some_and(|row| row.value.trim_end() != ui.composer.text().trim_end())
}

/// The row the dropdown's cursor is on.
fn chosen(ui: &Ui, tree: &Tree) -> Option<commands::Suggestion> {
    ui.selected_suggestion(cwd(tree), &mentions::targets(tree))
}

fn accept(ui: &mut Ui, tree: &Tree) -> Vec<Effect> {
    if let Some(chosen) = chosen(ui, tree) {
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
        KeyCode::Char('v') => return vec![Effect::PasteImage],
        // Claude Code's own key for its own list (M74): the rows go off the
        // band and come back, and nothing opens.
        KeyCode::Char('t') => ui.activity_hidden = !ui.activity_hidden,
        _ => {}
    }
    Vec::new()
}

fn alt(ui: &mut Ui, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Char('b') => ui.composer.word_left(),
        KeyCode::Char('f') => ui.composer.word_right(),
        KeyCode::Enter => newline(ui),
        _ => return crate::component::band::hotkey(ui, key).unwrap_or_default(),
    }
    Vec::new()
}

fn plain(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    match key.code {
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => newline(ui),
        KeyCode::Enter => return enter(ui, tree, now),
        KeyCode::BackTab => return mode::cycle_mode(ui, tree.viewed(), now),
        KeyCode::Up => return walk_back(ui, tree),
        // Down returns through the draft/history walk. Ctrl+r opens sessions.
        KeyCode::Down => walk_forward(ui),
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
        KeyCode::Char('/') if ui.composer.is_empty() => {
            return crate::palette::open(ui, bingo_sdk::component::PaletteOpen::Commands, now);
        }
        KeyCode::Char('?') if ui.composer.is_empty() => {
            return command_line(ui, tree, now, "/help".into());
        }
        KeyCode::Char('v') if reading(ui) => start_selection(ui),
        // A key belongs to the focused card's buttons while one is focused and
        // nothing is being typed; the rest of the time it is a letter.
        KeyCode::Char(c) if ui.composer.is_empty() && ui.focus.row().is_some() => {
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
    let delivery = if ui.dialog.aside && tree.open_interaction().is_some() {
        Delivery::Hold
    } else {
        Delivery::Wake
    };
    submit(ui, tree, now, delivery)
}

/// A line a key or a panel runs for the person: it does what their typed
/// line would, and their history and their draft stay as they were.
pub(crate) fn command_line(ui: &mut Ui, tree: &Tree, now: Now, line: String) -> Vec<Effect> {
    if let Some(effects) = crate::palette::command(ui, &line, now) {
        return effects;
    }
    send(ui, tree, now, line, Effect::Run)
}

/// A line the person composed in the palette: theirs, so the history keeps
/// it as if `⏎` had sent it from the box.
pub(crate) fn typed_line(ui: &mut Ui, tree: &Tree, now: Now, line: String) -> Vec<Effect> {
    if let Some(effects) = crate::palette::command(ui, &line, now) {
        return effects;
    }
    ui.history.remember(&line);
    send(ui, tree, now, line, |text| {
        own_line(text, Delivery::default())
    })
}

/// The person's own line: the one the history keeps.
///
/// `delivery` is the whole of the difference between the two keys that send:
/// `⏎` steers a running turn at its next barrier, `tab` waits for it to end.
fn submit(ui: &mut Ui, tree: &Tree, now: Now, delivery: Delivery) -> Vec<Effect> {
    let text = ui.composer.take();
    ui.history.remember(&text);
    ui.edited();
    send(ui, tree, now, text, |text| own_line(text, delivery))
}

/// The pictures — the tokens the line still names and the `@word`s in it —
/// are files, read by the loop, which knows the directory and what is held
/// (`run::submit`). The loop also writes the line to the history file.
fn own_line(text: String, delivery: Delivery) -> Effect {
    Effect::Submit(Input::Text {
        images: Vec::new(),
        text,
        origin: crate::origin(),
        delivery,
    })
}

/// What a line does, whoever wrote it; `line` is how it leaves when it is
/// neither a palette command nor one this surface answers itself. `/clear`
/// starts a fresh session beside the root's, not beside whichever child is
/// on screen.
fn send(
    ui: &mut Ui,
    tree: &Tree,
    now: Now,
    text: String,
    line: impl FnOnce(String) -> Effect,
) -> Vec<Effect> {
    // The gesture is answered on its own frame, whatever the line turns out
    // to be and whatever the kernel makes of it (§6).
    ui.sent = Some(now.instant);
    ui.scroll.end();
    if let Some(effects) = crate::palette::command(ui, &text, now) {
        return effects;
    }
    match commands::local(&text) {
        Some(Local::Clear) => vec![Effect::Open(SessionSelector::Create {
            spec: SessionSpec {
                cwd: PathBuf::from(&tree.root().summary.cwd),
                ..SessionSpec::default()
            },
        })],
        Some(Local::Exit) => vec![Effect::Exit],
        None => vec![line(text)],
    }
}

/// `↑` walks back through what a person has sent: the caret while it can
/// still move, then the lines of theirs the queue is still holding — newest
/// first, each taken back out of it and into the box — and only then the
/// prompt history (M68). A line still queued is still theirs to edit; one a
/// turn has already taken is in the history like every other line.
fn walk_back(ui: &mut Ui, tree: &Tree) -> Vec<Effect> {
    if ui.composer.up() {
        return Vec::new();
    }
    if let Some(intent) = newest_of_ours(tree.viewed()) {
        return vec![Effect::Withdraw(intent)];
    }
    let older = ui.older();
    recall(ui, older);
    Vec::new()
}

/// The last line this surface put in the queue, which is the first one `↑`
/// asks for back. Another surface's is not ours to take (ADR-0008 §2), so it
/// is not offered here either.
fn newest_of_ours(state: &SessionState) -> Option<IntentId> {
    state
        .queue
        .iter()
        .rfind(|entry| entry.origin.surface == SURFACE_ID)
        .map(|entry| entry.intent.clone())
}

/// `↓` walks the buffer until it runs out, then forward through the history
/// and back to the draft the walk set aside. It passes no queue: what came
/// out of one is in the box, and a line is only ever put back by sending it.
fn walk_forward(ui: &mut Ui) {
    if ui.composer.down() {
        return;
    }
    let newer = ui.history.newer();
    recall(ui, newer);
}

/// A recalled line is not typed: the dropdown stays closed over it, so the
/// next `↑` walks on past a `/…` line and `enter` runs it as it was, and
/// typing opens the dropdown again as after `esc`.
fn recall(ui: &mut Ui, recalled: Option<String>) {
    if let Some(text) = recalled {
        ui.composer.set(&text);
        ui.menu = Menu {
            selected: 0,
            dismissed: true,
        };
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
    use crate::pointer::{WHEEL, on_mouse};
    use crate::test_support::*;
    use bingo_sdk::{Activation, Answer, TurnId, TurnStatus};
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

    /// Any key skips the opening, and does nothing else with itself: the `/`
    /// that cut the piece short does not also open the dropdown, and the `d`
    /// that cut it short is not typed (M70, design §11).
    #[test]
    fn any_key_skips_the_opening_and_is_spent_on_nothing_else() {
        for key in [
            typed('/'),
            typed('d'),
            key(KeyCode::Esc),
            key(KeyCode::Enter),
            ctrl('c'),
        ] {
            let state = state();
            let tree = solo(&state);
            let (mut ui, now) = scene();
            ui.focus = crate::focus::Focus::Intro(crate::opening::Playing::from(now.instant));
            let effects = on_key(&mut ui, &tree, key, now);
            assert!(ui.focus.intro().is_none(), "{key:?} skipped it");
            assert!(effects.is_empty(), "{key:?} did nothing else: {effects:?}");
            assert_eq!(ui.composer.text(), "", "{key:?} typed nothing");
            assert!(ui.suggestions(&state.summary.cwd, &[]).is_empty());
        }
    }

    /// And the next key is a key again.
    #[test]
    fn the_key_after_the_skip_is_typed() {
        let state = state();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        ui.focus = crate::focus::Focus::Intro(crate::opening::Playing::from(now.instant));
        on_key(&mut ui, &tree, typed('h'), now);
        on_key(&mut ui, &tree, typed('i'), now);
        assert_eq!(ui.composer.text(), "i");
    }

    #[test]
    fn an_at_sign_offers_the_paths_under_the_session_and_enter_takes_one() {
        let (_dir, state) = with_files();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        write(&mut ui, &state, "@Car", now);
        assert_eq!(
            ui.suggestions(&state.summary.cwd, &[])
                .iter()
                .map(|row| row.label.clone())
                .collect::<Vec<_>>(),
            vec!["@Cargo.toml".to_string()],
        );
        on_key(&mut ui, &tree, key(KeyCode::Enter), now);
        assert_eq!(ui.composer.text(), "@Cargo.toml ");
        assert!(
            ui.suggestions(&state.summary.cwd, &[]).is_empty(),
            "a finished mention offers nothing more"
        );
    }

    /// A mention travels as words; the loop reads the file it names, since
    /// only the loop knows the session's directory.
    #[test]
    fn a_mentioned_picture_is_words_the_loop_reads() {
        let (_dir, state) = with_files();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        write(&mut ui, &state, "look at @shot.png", now);
        let effects = on_key(&mut ui, &tree, key(KeyCode::Enter), now);
        assert_eq!(
            effects,
            vec![Effect::Submit(Input::Text {
                text: "look at @shot.png".into(),
                images: Vec::new(),
                origin: crate::origin(),
                delivery: Delivery::Wake,
            })],
        );
    }

    /// `ctrl+v` asks the loop for the clipboard; the line is untouched until
    /// the loop has a picture to put in it.
    #[test]
    fn ctrl_v_asks_for_the_clipboard() {
        let (mut ui, now) = scene();
        let tree = solo(&state());
        let effects = on_key(&mut ui, &tree, ctrl('v'), now);
        assert_eq!(effects, vec![Effect::PasteImage]);
        assert!(ui.composer.is_empty());
    }

    /// The line goes as typed, tokens and all, and carries no picture of its
    /// own: every picture it names is a file the loop reads on its way out
    /// (`run::submit`, ADR-0052), so `⏎` here is words alone.
    #[test]
    fn a_line_with_tokens_goes_as_typed_and_its_pictures_are_read_later() {
        let (mut ui, now) = scene();
        let tree = solo(&state());
        ui.pictures
            .hold("", std::path::PathBuf::from("/pasted/one.png"));
        ui.composer.set("see [image 1]");
        let effects = on_key(&mut ui, &tree, key(KeyCode::Enter), now);
        assert_eq!(
            effects,
            vec![Effect::Submit(Input::Text {
                text: "see [image 1]".into(),
                images: Vec::new(),
                origin: crate::origin(),
                delivery: Delivery::Wake,
            })],
        );
    }

    /// A transcript of two turns, and a session that can rewind to one.
    fn rewindable() -> (SessionState, Vec<bingo_sdk::CommandSpec>) {
        let mut state = state();
        state.transcript = vec![
            in_turn(
                "itm_1",
                "trn_1",
                user("itm_1", "what is in this workspace?"),
            ),
            in_turn("itm_2", "trn_2", user("itm_2", "write me a note")),
        ]
        .into_iter()
        .collect();
        (
            state,
            // The spec `bingo-checkpoints` registers (ADR-0045 §3): a
            // surface offers what the catalogue says is there.
            vec![bingo_sdk::CommandSpec {
                opens: false,
                keys: vec![],
                hidden: false,
                name: "rewind".into(),
                aliases: Vec::new(),
                hint: "go back to a turn: its files and the conversation".into(),
                args: bingo_sdk::ArgSpec::Free {
                    hint: "<turn>".into(),
                },
                instant: false,
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
        ui.catalogs.commands = crate::commands::entries(commands);
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(ui.esc_armed, "the first one arms it and closes nothing");
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(ui.focus.is(&Open::Rewind(Rewind::default())));

        on_key(&mut ui, &tree, key(KeyCode::Down), now);
        let effects = on_key(&mut ui, &tree, key(KeyCode::Enter), now);
        assert_eq!(
            effects,
            vec![Effect::Submit(Input::text(
                "/rewind trn_1",
                crate::origin(),
            ))],
            "the second row is the older turn"
        );
    }

    #[test]
    fn a_key_between_the_two_escapes_is_what_says_they_were_not_one_gesture() {
        let (state, commands) = rewindable();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        ui.catalogs.commands = crate::commands::entries(commands);
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        on_key(&mut ui, &tree, typed('a'), now);
        assert!(!ui.esc_armed);
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(!ui.focus.showing(), "and a half-typed line is not empty");
    }

    /// The picker is offered only where the session has a `/rewind` to run.
    /// Since M67 the checkpoints plugin registers one, so this is a host
    /// built without it rather than the empty catalogue of the day.
    #[test]
    fn nothing_opens_where_the_session_cannot_rewind() {
        let (state, _) = rewindable();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        on_key(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(!ui.focus.showing());
        assert!(ui.notice.is_none(), "and it is silent about it");
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
                vec![Effect::Submit(Input::text(text, crate::origin()))],
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

    // ---- the permission mode --------------------------------------------

    /// The cycle types only a `/permission` line naming a mode word. A mode
    /// whose placer made it name another command, or smuggle more into the
    /// line, goes as the placer's word, never as the person's typing.
    #[test]
    fn shift_tab_types_only_a_mode_word_and_signs_the_rest_as_the_placer() {
        let placed = |next: bingo_sdk::Action| {
            let item = |label: &str, action| bingo_sdk::ActionItem {
                label: label.into(),
                action,
                key: None,
            };
            let mode = bingo_sdk::slot::Slot::Mode {
                current: "now".into(),
                modes: vec![
                    item(
                        "now",
                        bingo_sdk::Action {
                            name: "permission".into(),
                            args: "now".into(),
                        },
                    ),
                    item("next", next),
                ],
            };
            folded(vec![frame(
                1,
                bingo_sdk::Event::ConfigChanged {
                    config: bingo_sdk::ConfigView {
                        plugins: std::collections::BTreeMap::from([(
                            "policy".to_string(),
                            bingo_sdk::slot::with(serde_json::json!({}), [mode]),
                        )]),
                        ..Default::default()
                    },
                },
            )])
        };
        for next in [
            bingo_sdk::Action {
                name: "plugins".into(),
                args: "trust evil full".into(),
            },
            bingo_sdk::Action {
                name: "permission".into(),
                args: "plan\n/plugins trust evil".into(),
            },
            bingo_sdk::Action {
                name: "permission".into(),
                args: serde_json::json!({"mode":"plan"}),
            },
        ] {
            let (mut ui, now) = scene();
            let effects = press(&mut ui, &placed(next.clone()), shift(KeyCode::BackTab), now);
            assert_eq!(
                effects,
                vec![Effect::Submit(bingo_sdk::Input::action(
                    next,
                    bingo_sdk::Origin::surface("plugin:policy")
                ))]
            );
        }
    }

    #[test]
    fn shift_tab_cycles_only_the_policys_own_modes() {
        let mut policy = with_permission_mode("default");
        let forged = bingo_sdk::slot::Slot::Mode {
            current: "acme".into(),
            modes: ["acme", "bypassPermissions"]
                .map(|mode| bingo_sdk::ActionItem {
                    label: mode.into(),
                    key: None,
                    action: bingo_sdk::Action {
                        name: "permission".into(),
                        args: mode.into(),
                    },
                })
                .to_vec(),
        };
        policy.signals.entry("acme".into()).or_default().insert(
            "mode".into(),
            bingo_sdk::slot::with(serde_json::json!({}), [forged.clone()]).into(),
        );
        let described = bingo_sdk::slot::with(serde_json::json!({}), [forged.clone()]);
        policy
            .config
            .plugins
            .insert("aaa".into(), described.clone());
        let (mut ui, now) = scene();
        assert_eq!(
            press(&mut ui, &policy, shift(KeyCode::BackTab), now),
            vec![Effect::Run("/permission acceptEdits".into())]
        );
        let mut unnamed = state();
        unnamed.config.plugins.insert("aaa".into(), described);
        let (mut ui, now) = scene();
        assert!(
            !press(&mut ui, &unnamed, shift(KeyCode::BackTab), now)
                .iter()
                .any(|effect| matches!(effect, Effect::Run(_))),
            "a config slice the kernel did not name as the policy never types for the person"
        );
        let mut alone = state();
        alone.signals.entry("acme".into()).or_default().insert(
            "mode".into(),
            bingo_sdk::slot::with(serde_json::json!({}), [forged]).into(),
        );
        let (mut ui, now) = scene();
        assert_eq!(
            press(&mut ui, &alone, shift(KeyCode::BackTab), now),
            vec![Effect::Submit(bingo_sdk::Input::action(
                bingo_sdk::Action {
                    name: "permission".into(),
                    args: "bypassPermissions".into(),
                },
                bingo_sdk::Origin::surface("plugin:acme")
            ))]
        );
    }

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
                vec![Effect::Run(format!("/permission {next}"))],
                "{mode}"
            );
            assert!(ui.notice.is_none(), "{mode}");
        }
    }

    #[test]
    fn shift_tab_says_so_when_the_mode_is_not_one_it_knows() {
        for state in [state(), with_permission_mode("acceptedits")] {
            let (mut ui, now) = scene();
            assert!(press(&mut ui, &state, shift(KeyCode::BackTab), now).is_empty());
            assert!(ui.notice().iter().any(|n| n.text == UNKNOWN_MODE));
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
        let line = crate::keybar::hints(&solo(&state()), &ui, 80, now, &mut Vec::new()).to_string();
        assert!(line.contains("ctrl+c again to exit"), "{line}");
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

    /// Both keys that stop a turn leave the same mark, and it names the turn
    /// they stopped: the activity row reads it on this very frame, and the
    /// turn after this one is somebody else's business.
    #[test]
    fn stopping_a_turn_marks_the_turn_it_stopped() {
        for stop in [key(KeyCode::Esc), ctrl('c')] {
            let (mut ui, now) = scene();
            assert_eq!(ui.stop_asked, None);
            assert_eq!(press(&mut ui, &busy(), stop, now), vec![Effect::Interrupt]);
            assert_eq!(ui.stop_asked, Some(TurnId::from_raw("trn_1")));
        }
    }

    /// An `esc` that closed a layer is not an interrupt, and marks nothing.
    #[test]
    fn an_esc_that_closes_something_stops_no_turn() {
        let (mut ui, now) = scene();
        let busy = busy();
        write(&mut ui, &busy, "/he", now);
        assert!(press(&mut ui, &busy, key(KeyCode::Esc), now).is_empty());
        assert_eq!(ui.stop_asked, None, "the dropdown closed, the turn ran on");
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

    /// One card, one key path (M53): the same routing that answers a
    /// permission walks a form's tabs and sends every answer at once.
    #[test]
    fn a_form_is_answered_once_for_all_of_its_questions() {
        let state = folded(vec![frame(1, opened(crate::test_support::form()))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        // Three questions fixed, and the walk lands on the tab that sends
        // (M57): nothing leaves the card until `⏎` is pressed there.
        for key in [
            key(KeyCode::Enter),
            key(KeyCode::Enter),
            typed(' '),
            key(KeyCode::Enter),
        ] {
            assert!(
                press(&mut ui, &state, key, now).is_empty(),
                "nothing is sent until the whole form is"
            );
        }
        assert_eq!(
            press(&mut ui, &state, key(KeyCode::Enter), now),
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::Form {
                    answers: vec![
                        Answer::Choice {
                            ids: vec!["0".into()],
                            other: None,
                        },
                        Answer::Choice {
                            ids: vec!["0".into()],
                            other: None,
                        },
                        Answer::Choice {
                            ids: vec!["0".into()],
                            other: None,
                        },
                    ],
                },
                activation: Activation::Keyboard,
            }]
        );
    }

    /// `esc` is the card's rung of the one stack, and a form's whole set is
    /// what it leaves (§7).
    #[test]
    fn esc_leaves_the_whole_form() {
        let state = folded(vec![frame(1, opened(crate::test_support::form()))]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        press(&mut ui, &state, key(KeyCode::Enter), now);
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
                    ids: vec!["o".into()],
                    other: None,
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
                    ids: vec!["a".into()],
                    other: None,
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

    /// A line a panel ran is not one the person wrote: `↑` never offers it
    /// back, the loop writes it to no history file, and the draft stays.
    #[test]
    fn a_line_a_panel_runs_stays_out_of_the_history() {
        let (mut ui, now) = scene();
        line(&mut ui, &state(), "my own ask", now);
        write(&mut ui, &state(), "a draft", now);
        let effects = command_line(&mut ui, &solo(&state()), now, "/mcp approve db".into());
        assert_eq!(effects, vec![Effect::Run("/mcp approve db".into())]);
        assert_eq!(ui.composer.text(), "a draft");
        ui.composer.set("");
        press(&mut ui, &state(), key(KeyCode::Up), now);
        assert_eq!(
            ui.composer.text(),
            "my own ask",
            "the newest line is theirs"
        );
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
    fn a_release_event_is_not_a_press() {
        let (mut ui, now) = scene();
        let mut key = typed('x');
        key.kind = crossterm::event::KeyEventKind::Release;
        press(&mut ui, &state(), key, now);
        assert!(ui.composer.is_empty());
    }

    // ---- the switcher ---------------------------------------------------

    // ---- the list is typed into (M55, re-cut by M58) ---------------------

    // ---- `tab` completes (M58) ------------------------------------------

    // ---- one list, two doors --------------------------------------------

    /// `esc` on the list closes the list and nothing else: it is the one thing
    /// a person opened, so it is the one thing that press takes away.
    #[test]
    fn esc_on_the_list_does_not_reach_the_turn() {
        let tree = with_child(vec![frame(2, started("trn_1"))]);
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, key(KeyCode::Down), now);
        press_tree(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(!ui.focus.showing());
        assert_eq!(
            press_tree(&mut ui, &tree, key(KeyCode::Esc), now),
            vec![Effect::Interrupt],
            "and the next one is the turn's again"
        );
    }

    #[test]
    fn down_keeps_its_old_meaning_where_there_is_nowhere_to_walk_to() {
        let (mut ui, now) = scene();
        line(&mut ui, &state(), "the first thing", now);
        press(&mut ui, &state(), key(KeyCode::Up), now);
        assert_eq!(ui.composer.text(), "the first thing");
        press(&mut ui, &state(), key(KeyCode::Down), now);
        assert!(ui.composer.is_empty(), "the walk came back to the draft");
        assert!(!ui.focus.showing(), "and a session alone opened no list");
    }

    #[test]
    fn a_half_typed_line_keeps_the_arrows_for_itself() {
        let tree = with_child(vec![]);
        let (mut ui, now) = scene();
        write(&mut ui, tree.viewed(), "half a thought", now);
        press_tree(&mut ui, &tree, key(KeyCode::Down), now);
        assert!(!ui.focus.showing());
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
        render(&state, &mut ui, now);
        press(&mut ui, &state, key(KeyCode::PageUp), now);
        render(&state, &mut ui, now);
        press(&mut ui, &state, typed('v'), now);
        assert!(
            ui.focus.selection().is_some(),
            "v starts one while reading back"
        );

        press(&mut ui, &state, key(KeyCode::Down), now);
        press(&mut ui, &state, key(KeyCode::Right), now);
        let copied = press(&mut ui, &state, typed('y'), now);
        let [Effect::Copy(text)] = copied.as_slice() else {
            panic!("y copies: {copied:?}")
        };
        assert_eq!(
            text.split('\n').count(),
            2,
            "a run of two lines, cut where the far end is: {text:?}"
        );
        assert!(ui.focus.selection().is_none(), "copying lets it go");
    }

    /// A page back, settled, so a run started at the top of the screen has
    /// transcript on either side of it.
    fn reading_back(state: &SessionState) -> (Ui, Now) {
        let (mut ui, now) = scene();
        render(state, &mut ui, now);
        press(&mut ui, state, key(KeyCode::PageUp), now);
        let now = settled_at(now);
        render(state, &mut ui, now);
        (ui, now)
    }

    /// The frame after an ease has run out: what the screen shows once the
    /// transcript has arrived where a key sent it.
    fn settled_at(now: Now) -> Now {
        later(now, crate::scroll::EASE.as_millis() as i64)
    }

    /// The arrows reach the whole transcript, so the view goes with them: a
    /// far end walked one line past the last row on the screen brings that
    /// line onto it, and nothing else moves.
    #[test]
    fn walking_the_far_end_off_the_screen_brings_the_view_after_it() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let (_, rows) = ui.transcript();
        let top = ui.shown.top;
        press(&mut ui, &state, typed('v'), now);
        assert_eq!(
            ui.focus.selection().map(|run| run.head.line),
            Some(top),
            "a run starts at the first line on the screen"
        );
        for _ in 0..rows {
            press(&mut ui, &state, key(KeyCode::Down), now);
        }
        let now = settled_at(now);
        render(&state, &mut ui, now);
        let painted = &ui.shown;
        assert_eq!(painted.top, top + 1, "one line, and only one");
        assert_eq!(
            painted.row_of(top + rows),
            u16::try_from(rows - 1).ok(),
            "the far end is on the last row of the screen"
        );
    }

    #[test]
    fn walking_it_off_the_top_brings_the_view_back_up() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let top = ui.shown.top;
        assert!(top > 0, "there is transcript above the screen");
        press(&mut ui, &state, typed('v'), now);
        press(&mut ui, &state, key(KeyCode::Up), now);
        let now = settled_at(now);
        render(&state, &mut ui, now);
        let painted = &ui.shown;
        assert_eq!(painted.top, top - 1);
        assert_eq!(
            painted.row_of(top - 1),
            Some(0),
            "the far end is on the first row of the screen"
        );
    }

    /// Sideways is not a reason to move: a view that jumped on every `→`
    /// would chase a column the screen already shows.
    #[test]
    fn walking_the_far_end_across_a_line_moves_nothing() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let top = ui.shown.top;
        press(&mut ui, &state, typed('v'), now);
        press(&mut ui, &state, key(KeyCode::Right), now);
        let now = settled_at(now);
        render(&state, &mut ui, now);
        assert_eq!(ui.shown.top, top);
    }

    #[test]
    fn a_letter_ends_a_run_and_is_typed() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
        press(&mut ui, &state, key(KeyCode::PageUp), now);
        render(&state, &mut ui, now);
        press(&mut ui, &state, typed('v'), now);
        press(&mut ui, &state, typed('h'), now);
        assert!(ui.focus.selection().is_none());
        assert_eq!(ui.composer.text(), "h");
    }

    #[test]
    fn v_is_a_letter_while_the_transcript_is_at_its_foot() {
        let (mut ui, now) = scene();
        write(&mut ui, &state(), "ver", now);
        assert!(
            ui.focus.selection().is_none(),
            "a message may start with one"
        );
        assert_eq!(ui.composer.text(), "ver");
    }

    #[test]
    fn esc_lets_a_run_go() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
        press(&mut ui, &state, key(KeyCode::PageUp), now);
        render(&state, &mut ui, now);
        press(&mut ui, &state, typed('v'), now);
        assert!(press(&mut ui, &state, key(KeyCode::Esc), now).is_empty());
        assert!(ui.focus.selection().is_none());
    }

    /// A hand still out past the edge is let go with the run it was drawing:
    /// nothing walks the view afterwards.
    #[test]
    fn esc_lets_go_of_a_hand_held_past_the_edge() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let tree = solo(&state);
        let foot = below_the_transcript(&ui, 1);
        on_mouse(&mut ui, &tree, click(2, 4), now);
        on_mouse(&mut ui, &tree, dragged(4, foot), now);
        assert!(ui.select.dragging.is_some());
        press(&mut ui, &state, key(KeyCode::Esc), now);
        assert!(ui.focus.selection().is_none(), "esc lets it go");
        assert!(ui.select.dragging.is_none(), "and the hand with it");
    }

    // ---- the mouse ------------------------------------------------------

    #[test]
    fn the_wheel_scrolls_the_transcript_and_the_foot_takes_it_back() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
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

    /// The way back to the foot (M95) answers a click as `end` answers a
    /// key, and only on its own cells: the air beside it is still air.
    #[test]
    fn a_click_on_the_foot_row_follows_the_tail_again() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
        assert!(ui.shown.foot.is_none(), "following draws no pill");
        press(&mut ui, &state, key(KeyCode::PageUp), now);
        let settled = later(now, 100);
        render(&state, &mut ui, settled);
        let pill = ui.shown.foot.expect("a held transcript draws the pill");

        on_mouse(&mut ui, &solo(&state), click(pill.x - 1, pill.y), settled);
        assert_ne!(
            ui.scroll,
            crate::scroll::Scroll::Tail,
            "the air beside it is air"
        );

        on_mouse(&mut ui, &solo(&state), click(pill.x + 1, pill.y), settled);
        assert_eq!(ui.scroll, crate::scroll::Scroll::Tail);
        render(&state, &mut ui, settled);
        assert!(ui.shown.foot.is_none(), "at the foot the row is air again");
    }

    #[test]
    fn a_click_in_the_transcript_focuses_the_block_it_landed_on() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
        // Row 17 is the transcript's last: the reserved activity band holds
        // the two rows beneath it (view.rs's demand).
        let last = ui.shown.regions.transcript.bottom() - 1;
        on_mouse(&mut ui, &solo(&state), click(4, last), now);
        assert_eq!(
            ui.select.block,
            Some(bingo_sdk::ItemId::from_raw("itm_59")),
            "the last row is the last item"
        );
        assert!(ui.focus.selection().is_some(), "and a run starts there");
    }

    /// A block with more under it than a row can hold.
    fn long_result() -> SessionState {
        let output =
            bingo_sdk::ToolOutput::text((1..=9).map(|i| format!("line {i}\n")).collect::<String>());
        folded(vec![frame(
            1,
            bingo_sdk::Event::ItemCompleted {
                item: tool(
                    "itm_1",
                    "Read",
                    serde_json::json!({"file_path": "src/lib.rs"}),
                    Some(output),
                    bingo_sdk::ItemStatus::Completed,
                ),
            },
        )])
    }

    /// The last row the transcript drew, which is the row a fold sits on.
    fn last_row(ui: &Ui) -> u16 {
        ui.shown.regions.transcript.y
            + ui.shown
                .height
                .min(usize::from(ui.shown.regions.transcript.height)) as u16
            - 1
    }

    /// A click on a fold is one gesture that opens it and folds it again — the
    /// set `ctrl+o` fills, reached from the mouse (design §7).
    #[test]
    fn a_click_opens_a_fold_and_a_second_click_folds_it() {
        let state = long_result();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        assert!(render(&state, &mut ui, now).contains("+4 lines (ctrl+o to expand)"));

        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        let opened = render(&state, &mut ui, now);
        assert!(opened.contains("line 9"), "{opened}");
        assert!(!opened.contains("+4 lines"), "{opened}");
        assert_eq!(
            ui.select.block,
            Some(bingo_sdk::ItemId::from_raw("itm_1")),
            "and the click still takes the focus"
        );

        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        assert!(
            render(&state, &mut ui, now).contains("+4 lines (ctrl+o to expand)"),
            "the same gesture on the same row takes it back"
        );
    }

    /// A notice folds without promising a key (2026-09-01), so the click is
    /// the only way in — and it is the way in for every fold, not just this one.
    #[test]
    fn a_click_opens_a_folded_notice_too() {
        let body: String = (1..=9).map(|i| format!("\nline {i}")).collect();
        let state = folded(vec![frame(
            1,
            bingo_sdk::Event::ItemCompleted {
                item: delivered(
                    "itm_1",
                    "schedule",
                    None,
                    &format!("the nightly run is in{body}"),
                ),
            },
        )]);
        let tree = solo(&state);
        let (mut ui, now) = scene();
        assert!(render(&state, &mut ui, now).contains("+4 lines"));
        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        let opened = render(&state, &mut ui, now);
        assert!(opened.contains("line 9"), "{opened}");
        assert!(!opened.contains("+4 lines"), "{opened}");
    }

    /// A thought, once it is one.
    fn thought(text: &str) -> SessionState {
        let mut item = crate::test_support::item(
            "itm_1",
            bingo_sdk::ItemStatus::Completed,
            bingo_sdk::ItemBody::Reasoning {
                text: text.into(),
                provider_metadata: Default::default(),
            },
        );
        item.completed_at = Some(ts() + jiff::SignedDuration::from_secs(2));
        folded(vec![frame(1, bingo_sdk::Event::ItemCompleted { item })])
    }

    /// The steps a thought that is over takes, deep enough that each one shows
    /// something the last one did not.
    fn steps() -> String {
        (1..=9).map(|i| format!("step {i}\n")).collect()
    }

    /// `ctrl+o` only ever opens further (§7): a thought that is over holds the
    /// two rows it ended on, and from there the whole of it and then the
    /// sheet. `esc` out of the sheet puts it back where its kind starts.
    #[test]
    fn ctrl_o_climbs_a_thought_from_its_hold_through_the_whole_to_the_sheet() {
        let state = thought(&steps());
        let (mut ui, now) = scene();
        let held = render(&state, &mut ui, now);
        assert!(held.contains("Thought for 2s"), "{held}");
        assert!(held.contains("step 9"), "it holds where it ended: {held}");
        assert!(!held.contains("step 7"), "two rows of it: {held}");
        assert!(
            !held.contains("ctrl+o to expand"),
            "and no cut mark: {held}"
        );

        press(&mut ui, &state, ctrl('o'), now);
        let opened = render(&state, &mut ui, now);
        assert!(opened.contains("step 1"), "{opened}");

        press(&mut ui, &state, ctrl('o'), now);
        assert!(
            render(&state, &mut ui, later(now, 200)).contains("Thinking"),
            "the sheet says what it is"
        );

        press(&mut ui, &state, key(KeyCode::Esc), now);
        assert!(ui.folds.is_empty(), "and esc folds it back to the hold");
    }

    /// A click walks the same rungs and comes back round (§7): a thought that
    /// is over is the one block with three states, because it is the one whose
    /// working a person may want put away — and putting it away is on the far
    /// side of the whole, never where the thought was met.
    #[test]
    fn a_click_cycles_a_finished_thought_through_its_three_states() {
        let state = thought(&steps());
        let tree = solo(&state);
        let (mut ui, now) = scene();
        let held = render(&state, &mut ui, now);
        assert!(
            held.contains("step 9") && !held.contains("step 7"),
            "{held}"
        );

        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        let opened = render(&state, &mut ui, now);
        assert!(opened.contains("step 1"), "{opened}");
        assert!(!opened.contains("+7 lines"), "{opened}");

        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        let shut = render(&state, &mut ui, now);
        assert!(!shut.contains("step 9"), "the row alone: {shut}");

        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        assert!(
            render(&state, &mut ui, now).contains("step 9"),
            "and the fourth click is back where the first started"
        );
    }

    /// A run of thoughts is one block (M79), so it is one fold: a click on any
    /// of its rows opens the whole run where it happened, and `ctrl+o` from
    /// there takes a sheet with every thought of it.
    #[test]
    fn a_click_opens_the_whole_run_and_ctrl_o_takes_it_to_the_sheet() {
        let state = a_run_of_thoughts();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        let held = render(&state, &mut ui, now);
        assert!(
            held.contains("Thought for 6s"),
            "the run's own time: {held}"
        );
        assert!(
            !held.contains("The manifest first"),
            "the tail of the run alone: {held}"
        );

        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        let opened = render(&state, &mut ui, now);
        assert!(opened.contains("The manifest first"), "{opened}");
        assert_eq!(ui.folds.len(), 1, "one fold for the whole run");

        press(&mut ui, &state, ctrl('o'), now);
        let sheet = render(&state, &mut ui, later(now, 200));
        for thought in ["The manifest first", "Then the crate map", "The plan after"] {
            assert!(sheet.contains(thought), "{thought} is missing: {sheet}");
        }
    }

    /// Three thoughts in a row, as a provider that closes a reasoning item per
    /// output item hands them over.
    fn a_run_of_thoughts() -> SessionState {
        let thought = |id: &str, text: &str, seconds: i64| {
            let mut item = crate::test_support::item(
                id,
                bingo_sdk::ItemStatus::Completed,
                bingo_sdk::ItemBody::Reasoning {
                    text: text.into(),
                    provider_metadata: Default::default(),
                },
            );
            item.completed_at = Some(ts() + jiff::SignedDuration::from_secs(seconds));
            item
        };
        folded(vec![
            frame(
                1,
                bingo_sdk::Event::ItemCompleted {
                    item: thought("itm_1", "The manifest first.", 1),
                },
            ),
            frame(
                2,
                bingo_sdk::Event::ItemCompleted {
                    item: thought("itm_2", "Then the crate map.", 2),
                },
            ),
            frame(
                3,
                bingo_sdk::Event::ItemCompleted {
                    item: thought("itm_3", "The plan after that.", 3),
                },
            ),
        ])
    }

    /// A redacted thought promises nothing: no fold, no key, no sheet.
    #[test]
    fn an_empty_thought_opens_nothing() {
        let state = thought("");
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
        press(&mut ui, &state, ctrl('o'), now);
        assert!(ui.folds.is_empty(), "there is nothing to lift");
        ui.select.block = Some(bingo_sdk::ItemId::from_raw("itm_1"));
        press(&mut ui, &state, key(KeyCode::Enter), now);
        assert!(!ui.focus.showing(), "and nothing to open");
    }

    /// A thought being had is not a fold: its two tail rows scroll rather than
    /// cut, and promise no key, so `ctrl+o` walks past it — and past the whole
    /// run it ends, because a run of thoughts is one thought (M79). The fold
    /// it lifts once the run is over is the run's last item, which is the one
    /// with the block.
    #[test]
    fn ctrl_o_walks_past_a_run_whose_last_thought_is_still_being_had() {
        let mut state = thought(&steps());
        let mut being_had = crate::test_support::item(
            "itm_2",
            bingo_sdk::ItemStatus::Running,
            bingo_sdk::ItemBody::Reasoning {
                text: "and now the lockfile".into(),
                provider_metadata: Default::default(),
            },
        );
        state.apply(&frame(
            2,
            bingo_sdk::Event::ItemStarted {
                item: being_had.clone(),
            },
        ));
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
        press(&mut ui, &state, ctrl('o'), now);
        assert!(
            ui.folds.is_empty(),
            "the run is still being thought: nothing on the screen cuts"
        );

        being_had.status = bingo_sdk::ItemStatus::Completed;
        being_had.completed_at = Some(ts() + jiff::SignedDuration::from_secs(3));
        state.apply(&frame(
            3,
            bingo_sdk::Event::ItemCompleted { item: being_had },
        ));
        render(&state, &mut ui, now);
        press(&mut ui, &state, ctrl('o'), now);
        assert_eq!(
            ui.folds.keys().map(ToString::to_string).collect::<Vec<_>>(),
            vec!["itm_2".to_string()],
            "one fold for the run, on the item its block hangs from"
        );
    }

    /// `⏎` on a focused block still opens the pager, thought or result alike.
    #[test]
    fn enter_on_a_focused_thought_opens_the_sheet() {
        let state = thought("the manifest first");
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
        ui.select.block = Some(bingo_sdk::ItemId::from_raw("itm_1"));
        press(&mut ui, &state, key(KeyCode::Enter), now);
        assert!(matches!(*ui.focus.open(), Open::Pager(_)));
    }

    #[test]
    fn a_drag_takes_the_far_end_of_the_run_with_it() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
        on_mouse(&mut ui, &solo(&state), click(2, 15), now);
        on_mouse(&mut ui, &solo(&state), dragged(6, 17), now);
        let run = ui.focus.selection().expect("a run");
        assert_eq!(run.anchor.column, 2);
        assert_eq!(run.head.column, 6);
        assert_eq!(run.head.line, run.anchor.line + 2);
    }

    /// What a drag copies is the text those cells were showing: the run is
    /// measured against the rendered transcript, and a wide glyph counts for
    /// the two cells it is drawn in.
    #[test]
    fn a_drag_copies_the_cells_it_went_over() {
        for (text, from, to, copied) in
            [("run the tests", 3, 6, "run"), ("你好 warm", 3, 7, "你好")]
        {
            let state = folded(vec![frame(
                1,
                bingo_sdk::Event::ItemCompleted {
                    item: user("itm_1", text),
                },
            )]);
            let tree = solo(&state);
            let (mut ui, now) = scene();
            let row = row_carrying(&render(&state, &mut ui, now), text);
            on_mouse(&mut ui, &tree, click(from, row), now);
            on_mouse(&mut ui, &tree, dragged(to, row), now);
            assert_eq!(
                press(&mut ui, &state, typed('y'), now),
                vec![Effect::Copy(copied.to_string())],
                "dragging {from}..{to} over {text:?}"
            );
        }
    }

    // ---- the selection follows the hand (M92) -----------------------------

    /// The rows of the transcript region as the last frame cut them.
    fn region(ui: &Ui) -> ratatui::layout::Rect {
        ui.shown.regions.transcript
    }

    /// A screen row `past` rows below the last one the transcript was drawn
    /// on: the activity row, the input box, the status line.
    fn below_the_transcript(ui: &Ui, past: u16) -> u16 {
        let region = region(ui);
        region.y + region.height + past - 1
    }

    /// A drag past the foot takes the view with it, by as many lines as the
    /// pointer is rows past the region, and the run reaches the line that is
    /// now drawn there.
    #[test]
    fn a_drag_below_the_transcript_scrolls_by_the_overshoot() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let tree = solo(&state);
        let (_, rows) = ui.transcript();
        let top = ui.shown.top;
        on_mouse(&mut ui, &tree, click(4, 3), now);
        let foot = below_the_transcript(&ui, 3);
        on_mouse(&mut ui, &tree, dragged(4, foot), now);
        assert_eq!(
            ui.focus.selection().map(|run| run.head.line),
            Some(top + 3 + rows - 1),
            "the run reaches the line now drawn at the foot"
        );
        let now = settled_at(now);
        render(&state, &mut ui, now);
        assert_eq!(ui.shown.top, top + 3, "three rows past, three lines");
        assert!(ui.select.dragging.is_some(), "the hand is still out there");
    }

    /// Back inside the region, the run reaches the cell under the pointer and
    /// the view stops walking.
    #[test]
    fn a_drag_back_inside_the_transcript_stops_the_walk() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let tree = solo(&state);
        on_mouse(&mut ui, &tree, click(4, 3), now);
        let foot = below_the_transcript(&ui, 2);
        on_mouse(&mut ui, &tree, dragged(4, foot), now);
        assert!(ui.select.dragging.is_some());
        on_mouse(&mut ui, &tree, dragged(6, 5), now);
        assert!(ui.select.dragging.is_none(), "the hand came back");
        let top = ui.shown.top;
        ui.drag_step(later(now, 500));
        assert_eq!(ui.shown.top, top, "and nothing walks without it");
    }

    /// A hand that stays out there takes a line every [`crate::select::
    /// EDGE_PACE`], measured from the last one it took — the loop asks on
    /// every frame, and the frames are shorter than the pace.
    #[test]
    fn a_hand_held_past_the_foot_takes_a_line_every_pace() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let tree = solo(&state);
        on_mouse(&mut ui, &tree, click(4, 3), now);
        let foot = below_the_transcript(&ui, 1);
        on_mouse(&mut ui, &tree, dragged(4, foot), now);
        let reached = |ui: &Ui| ui.focus.selection().map(|run| run.head.line);
        let head = reached(&ui);
        ui.drag_step(later(now, 49));
        assert_eq!(reached(&ui), head, "not yet");
        ui.drag_step(later(now, 50));
        assert_eq!(reached(&ui), head.map(|line| line + 1), "a line");
        ui.drag_step(later(now, 60));
        assert_eq!(reached(&ui), head.map(|line| line + 1), "and one at a time");
        ui.drag_step(later(now, 100));
        assert_eq!(reached(&ui), head.map(|line| line + 2));
    }

    /// It stops at the transcript's own last line: a hand may go on holding,
    /// and the frames another step would cost are not owed.
    #[test]
    fn a_walk_past_the_foot_stops_at_the_last_line() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let tree = solo(&state);
        let (height, _) = ui.transcript();
        on_mouse(&mut ui, &tree, click(4, 3), now);
        let foot = below_the_transcript(&ui, 1);
        on_mouse(&mut ui, &tree, dragged(4, foot), now);
        for step in 1..=height {
            ui.drag_step(later(now, 50 * step as i64));
        }
        assert_eq!(
            ui.focus.selection().map(|run| run.head.line),
            Some(height - 1)
        );
        assert!(ui.select.dragging.is_none(), "there is nothing further");
    }

    /// The first row of the transcript is as far up as a terminal can report:
    /// a hand resting there walks the view back up a line at a time, and stops
    /// at the transcript's first line.
    #[test]
    fn a_hand_held_at_the_top_walks_the_view_back_up() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let tree = solo(&state);
        let top = ui.shown.top;
        assert!(top > 0, "there is transcript above the screen");
        let head_row = region(&ui).y;
        on_mouse(&mut ui, &tree, click(4, 3), now);
        on_mouse(&mut ui, &tree, dragged(4, head_row), now);
        assert_eq!(
            ui.focus.selection().map(|run| run.head.line),
            Some(top),
            "the run reaches the first line on the screen, and the view holds"
        );
        ui.drag_step(later(now, 50));
        assert_eq!(ui.focus.selection().map(|run| run.head.line), Some(top - 1));
        for step in 2..=top + 1 {
            ui.drag_step(later(now, 50 * step as i64));
        }
        assert_eq!(ui.focus.selection().map(|run| run.head.line), Some(0));
        assert!(ui.select.dragging.is_none(), "there is nothing further");
    }

    /// Releasing the button is the copy: what the hand drew goes to the
    /// terminal's clipboard, the run is let go, and the notice says how much.
    #[test]
    fn releasing_a_drag_copies_what_it_reached_and_says_how_much() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let tree = solo(&state);
        let (_, rows) = ui.transcript();
        let top = ui.shown.top;
        // Three rows past the foot, so three lines the screen was not showing
        // join the run; the first of them with anything on it is the proof.
        let hidden = ui
            .transcript_text()
            .get(top + rows..top + rows + 3)
            .unwrap_or_default()
            .iter()
            .map(|line| line.trim_end().to_string())
            .find(|line| !line.is_empty())
            .expect("a line below the screen at the press");
        let head_row = region(&ui).y;
        let foot = below_the_transcript(&ui, 3);
        on_mouse(&mut ui, &tree, click(0, head_row), now);
        on_mouse(&mut ui, &tree, dragged(40, foot), now);
        let copied = on_mouse(&mut ui, &tree, released(40, foot), now);
        let [Effect::Copy(text)] = copied.as_slice() else {
            panic!("releasing copies: {copied:?}")
        };
        assert!(
            text.contains(&hidden),
            "the run reached what the screen had not: {hidden:?} in {text:?}"
        );
        assert!(ui.focus.selection().is_none(), "copying lets it go");
        assert!(ui.select.dragging.is_none());
        assert_eq!(
            ui.notice().map(|notice| notice.text.clone()),
            Some(format!("copied {} lines", rows + 3))
        );
    }

    /// A press and a release on one cell is a click, and a click means what it
    /// has always meant: the block takes the focus and its fold goes round.
    #[test]
    fn a_press_and_a_release_on_one_cell_is_still_a_click() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let tree = solo(&state);
        on_mouse(&mut ui, &tree, click(4, 5), now);
        let folded = ui.folds.len();
        let copied = on_mouse(&mut ui, &tree, released(4, 5), now);
        assert!(copied.is_empty(), "a click copies nothing: {copied:?}");
        assert!(ui.notice().is_none(), "and says nothing");
        assert!(ui.select.block.is_some(), "the block is still focused");
        assert_eq!(
            ui.folds.len(),
            folded,
            "and the fold the press cycled stands"
        );
    }

    /// The wheel is a second hand: it moves the view under a run being drawn
    /// and the run stays where the first hand put it.
    #[test]
    fn the_wheel_scrolls_under_a_run_and_keeps_it() {
        let state = long_transcript(60);
        let (mut ui, now) = reading_back(&state);
        let tree = solo(&state);
        on_mouse(&mut ui, &tree, click(2, 4), now);
        on_mouse(&mut ui, &tree, dragged(6, 6), now);
        let run = ui.focus.selection();
        let top = ui.shown.top;
        on_mouse(&mut ui, &tree, wheel(true, 6, 6), now);
        let now = settled_at(now);
        render(&state, &mut ui, now);
        assert_eq!(ui.shown.top, top - WHEEL as usize);
        assert_eq!(
            ui.focus.selection(),
            run,
            "the run is the hand's, not the wheel's"
        );
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
        render_tree(&tree, &mut ui, now);
        let row = ui.shown.regions.transcript.y
            + ui.shown
                .height
                .min(usize::from(ui.shown.regions.transcript.height)) as u16
            - 1;
        assert_eq!(
            on_mouse(&mut ui, &tree, click(4, row), now),
            vec![Effect::View(child_id())],
            "the `↳` row belongs to the call that spawned it"
        );
        assert!(
            ui.focus.selection().is_none(),
            "stepping in is not selecting"
        );
    }

    // ---- a picture is a thing to click (M56) ------------------------------

    /// A `Read` that answered with a picture and nothing else (ADR-0040 §1).
    fn read_a_picture() -> SessionState {
        folded(vec![frame(
            1,
            bingo_sdk::Event::ItemCompleted {
                item: item(
                    "itm_1",
                    bingo_sdk::ItemStatus::Completed,
                    bingo_sdk::ItemBody::ToolCall {
                        call_id: "call_1".into(),
                        name: "Read".into(),
                        input: serde_json::json!({ "file_path": "shot.png" }),
                        output: Some(bingo_sdk::ToolOutput {
                            parts: vec![bingo_sdk::ContentPart::Image(
                                bingo_pictures::testing::png(100, 200),
                            )],
                            display: None,
                            is_error: false,
                        }),
                        progress: None,
                        duration_ms: Some(3),
                        quiet: false,
                    },
                ),
            },
        )])
    }

    /// The row of the screen a picture's cells are on.
    fn cells_row(screen: &str) -> u16 {
        row_carrying(screen, &crate::graphics::kitty::PLACEHOLDER.to_string())
    }

    fn journal_picture() -> crate::graphics::picture::Source {
        crate::graphics::picture::Source::Journal {
            item: bingo_sdk::ItemId::from_raw("itm_1"),
            part: 0,
        }
    }

    /// A click inside a drawn picture opens it: the cells under the pointer say
    /// which picture they are of, and the effect carries that source (M56).
    #[test]
    fn a_click_inside_a_picture_opens_it() {
        let state = read_a_picture();
        let (mut ui, now) = scene();
        crate::graphics::with(crate::graphics::drawing(), || {
            let screen = render(&state, &mut ui, now);
            let row = cells_row(&screen);
            let effects = on_mouse(&mut ui, &solo(&state), click(6, row), now);
            assert_eq!(effects, vec![Effect::OpenPicture(journal_picture())]);
            assert!(
                ui.focus.selection().is_none(),
                "opening a picture is not selecting"
            );
            assert!(ui.folds.is_empty(), "and it is not the block's fold");
        });
    }

    /// A click beside the picture is the block's, as it always was: the
    /// rectangle answers for its own cells and for nothing around them.
    #[test]
    fn a_click_beside_a_picture_is_still_the_blocks() {
        let state = read_a_picture();
        let (mut ui, now) = scene();
        crate::graphics::with(crate::graphics::drawing(), || {
            let screen = render(&state, &mut ui, now);
            let row = row_carrying(&screen, "Read(shot.png)");
            assert!(
                on_mouse(&mut ui, &solo(&state), click(4, row), now).is_empty(),
                "the row above the cells cycles the fold"
            );
            assert!(!ui.folds.is_empty(), "which is what it wrote");
            let cells = cells_row(&screen);
            // Ten cells from column 5, so column 60 is past the picture.
            assert!(
                on_mouse(&mut ui, &solo(&state), click(60, cells), now).is_empty(),
                "and so does a click to the right of them"
            );
        });
    }

    /// A terminal that draws no picture has no cells to click: the chip is
    /// words, and a click on it is the block's fold.
    #[test]
    fn a_chip_is_words_and_a_click_on_it_opens_nothing() {
        let state = read_a_picture();
        let (mut ui, now) = scene();
        let screen = render(&state, &mut ui, now);
        let row = row_carrying(&screen, "[image: image/png]");
        assert!(on_mouse(&mut ui, &solo(&state), click(6, row), now).is_empty());
    }

    /// The strip's thumbnails answer the same way, so the picture about to be
    /// sent is as clickable as the ones already said (M56).
    #[test]
    fn a_click_on_a_strips_thumbnail_opens_it() {
        let state = state();
        let (mut ui, now) = scene();
        let token = crate::test_support::drafted(&mut ui, bingo_pictures::testing::png(100, 100));
        crate::graphics::with(crate::graphics::drawing(), || {
            let screen = render(&state, &mut ui, now);
            let row = cells_row(&screen);
            let effects = on_mouse(&mut ui, &solo(&state), click(3, row), now);
            assert_eq!(
                effects,
                vec![Effect::OpenPicture(
                    crate::graphics::picture::Source::Draft { token }
                )]
            );
        });
    }

    /// The column a row's placeholder cells start in.
    fn cells_column(screen: &str, row: u16) -> u16 {
        let line = screen.lines().nth(usize::from(row)).expect("that row");
        let before = line
            .chars()
            .take_while(|c| *c != crate::graphics::kitty::PLACEHOLDER)
            .count();
        u16::try_from(before).expect("a column of the screen")
    }

    /// A person's own line with a picture behind its token (M45).
    fn sent_a_picture() -> SessionState {
        folded(vec![frame(
            1,
            bingo_sdk::Event::ItemCompleted {
                item: item(
                    "itm_1",
                    bingo_sdk::ItemStatus::Completed,
                    bingo_sdk::ItemBody::User {
                        parts: vec![
                            bingo_sdk::ContentPart::text("what is this? [image 1]"),
                            bingo_sdk::ContentPart::Image(bingo_pictures::testing::png(100, 200)),
                        ],
                        origin: crate::origin(),
                    },
                ),
            },
        )])
    }

    /// The band above a person's own line answers the same way (M62): the
    /// thumbnail is a smaller rectangle of the very same picture, so the cells
    /// under the pointer still say which one, and the click still opens it.
    #[test]
    fn a_click_on_the_band_above_a_persons_line_opens_it() {
        let state = sent_a_picture();
        let (mut ui, now) = scene();
        crate::graphics::with(crate::graphics::drawing(), || {
            let screen = render(&state, &mut ui, now);
            let row = cells_row(&screen);
            let column = cells_column(&screen, row);
            assert!(
                row < row_carrying(&screen, "what is this?"),
                "the band is above the words:\n{screen}"
            );
            let effects = on_mouse(&mut ui, &solo(&state), click(column, row), now);
            assert_eq!(effects, vec![Effect::OpenPicture(journal_picture())]);
            assert!(ui.folds.is_empty(), "and it is not the block's fold");
            // The thumbnail is three cells wide; past it the band is air, and
            // a click there is the block's as it always was.
            assert!(
                on_mouse(&mut ui, &solo(&state), click(column + 6, row), now).is_empty(),
                "and beside the thumbnail it is the block's again:\n{screen}"
            );
        });
    }

    #[test]
    fn a_click_on_a_card_row_answers_it() {
        let state = folded(vec![frame(1, opened(permission(Some("Edit(src/)"), None)))]);
        let (mut ui, now) = settled();
        ui.dialog.focus_on(state.interactions.first());
        render(&state, &mut ui, now);
        let card = ui.shown.card.clone().expect("a card on screen");
        let row = card
            .options
            .iter()
            .position(|option| option == &Some(1))
            .expect("the second option has a row");
        let effects = on_mouse(
            &mut ui,
            &solo(&state),
            click(4, card.area.y + row as u16),
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

    // ---- backgrounding the running command (ADR-0018 §6) ------------------

    /// A turn with a shell command in flight.
    fn running_bash() -> SessionState {
        crate::test_support::with_promotions(folded(vec![
            frame(1, started("trn_1")),
            frame(
                2,
                bingo_sdk::Event::ItemStarted {
                    item: running_tool("itm_1", "Bash", "compiling…"),
                },
            ),
        ]))
    }

    #[test]
    fn ctrl_b_fires_the_plugins_command_naming_the_call_that_is_running() {
        let (mut ui, now) = scene();
        let effects = press(&mut ui, &running_bash(), ctrl('b'), now);
        assert_eq!(
            effects,
            vec![Effect::Submit(bingo_sdk::Input::action(
                bingo_sdk::Action {
                    name: "bash.promote".into(),
                    args: serde_json::Value::String("call_1".into()),
                },
                bingo_sdk::Origin::surface("plugin:fixture.rows"),
            ))],
            "the row's command is its placer's word, not the person's"
        );
        assert!(ui.notice.is_none(), "the plugin answers, not the surface");
    }

    #[test]
    fn ctrl_b_says_so_when_no_shell_command_is_running() {
        for state in [
            state(),
            busy(),
            // A finished call is not a running one.
            crate::test_support::with_promotions(folded(vec![
                frame(1, started("trn_1")),
                frame(
                    2,
                    bingo_sdk::Event::ItemCompleted {
                        item: tool(
                            "itm_1",
                            "Bash",
                            serde_json::json!({ "command": "ls" }),
                            Some(bingo_sdk::ToolOutput::text("a\n")),
                            bingo_sdk::ItemStatus::Completed,
                        ),
                    },
                ),
            ])),
            // Another tool's call is not the shell's.
            crate::test_support::with_promotions(folded(vec![
                frame(1, started("trn_1")),
                frame(
                    2,
                    bingo_sdk::Event::ItemStarted {
                        item: running_tool("itm_1", "Read", "reading…"),
                    },
                ),
            ])),
        ] {
            let (mut ui, now) = scene();
            assert!(press(&mut ui, &state, ctrl('b'), now).is_empty());
            assert!(ui.notice().iter().any(|n| n.text == NOTHING_RUNNING));
        }
    }

    /// The key is in the one binding table, so the `?` sheet prints it.
    #[test]
    fn ctrl_b_is_documented_where_every_key_is() {
        let row = keys::BINDINGS
            .iter()
            .find(|binding| binding.keys == "ctrl+b")
            .expect("a row for ctrl+b");
        assert_eq!(row.description, "background running command");
    }

    /// Claude Code's key for its own list (M74): the rows go off the band and
    /// come back, and no layer opens over anything.
    #[test]
    fn ctrl_t_hides_the_task_list_and_again_shows_it() {
        let (mut ui, now) = scene();
        assert!(!ui.activity_hidden);
        assert!(press(&mut ui, &state(), ctrl('t'), now).is_empty());
        assert!(ui.activity_hidden);
        assert!(!ui.focus.showing(), "nothing opens");
        press(&mut ui, &state(), ctrl('t'), now);
        assert!(!ui.activity_hidden, "the same chord brings it back");
    }

    #[test]
    fn what_is_closing_stays_on_screen_until_it_has_gone() {
        let (mut ui, now) = scene();
        ui.focus.show(Open::Rewind(Default::default()), now.instant);
        press(&mut ui, &state(), key(KeyCode::Esc), now);
        assert!(!ui.focus.showing(), "it is on its way out");
        assert!(
            !ui.focus.reveal(now).gone(),
            "and still drawn while it goes"
        );
        ui.expire(frames_at(now, 4));
        assert_eq!(*ui.focus.open(), Open::Nothing);
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
            vec![Effect::Submit(Input::text("again", crate::origin()))]
        );
    }

    #[test]
    fn a_view_block_is_dismissed_by_the_next_key() {
        let (mut ui, now) = scene();
        ui.block = Some(bingo_sdk::View::text("x"));
        press(&mut ui, &state(), key(KeyCode::Left), now);
        assert!(ui.block.is_none());
    }

    #[test]
    fn the_page_keys_scroll_a_screenful_and_come_back_to_the_tail() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
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
        render(&state, &mut ui, now);
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

    #[test]
    fn a_digit_with_no_card_focused_is_typed() {
        let state = boarded();
        let (mut ui, now) = scene();
        assert!(press(&mut ui, &state, typed('1'), now).is_empty());
        assert_eq!(ui.composer.text(), "1");
    }

    // ---- the line that waits (M68) --------------------------------------

    /// A queue as the kernel published it, with the surface that put each
    /// line there and whether the running turn may be steered with it.
    fn queued(entries: &[(&str, &str)]) -> SessionState {
        folded(vec![
            frame(1, started("trn_1")),
            frame(
                2,
                bingo_sdk::Event::QueueChanged {
                    revision: 1,
                    entries: entries
                        .iter()
                        .enumerate()
                        .map(|(i, (surface, preview))| bingo_sdk::QueueEntry {
                            intent: IntentId::from_raw(format!("req_{i}")),
                            position: i as u32 + 1,
                            preview: (*preview).to_string(),
                            steerable: true,
                            origin: bingo_sdk::Origin::surface(*surface),
                        })
                        .collect(),
                },
            ),
        ])
    }

    fn delivery_of(effects: &[Effect]) -> Option<Delivery> {
        effects.iter().find_map(|effect| match effect {
            Effect::Submit(Input::Text { delivery, .. }) => Some(*delivery),
            _ => None,
        })
    }

    /// The user's second ask: `⏎` steers the running turn, `tab` sends a line
    /// that waits for it to end. One road out, one word different.
    #[test]
    fn tab_mid_turn_sends_a_line_that_waits_and_enter_sends_one_that_steers() {
        let (mut ui, now) = scene();
        write(&mut ui, &busy(), "and then the docs", now);
        let held = press(&mut ui, &busy(), key(KeyCode::Tab), now);
        assert_eq!(delivery_of(&held), Some(Delivery::Hold));
        assert!(ui.composer.is_empty(), "the box is cleared as by any send");

        let steer = line(&mut ui, &busy(), "no, this first", now);
        assert_eq!(delivery_of(&steer), Some(Delivery::Wake));
    }

    /// The three cases the risk register named, at the keyboard rather than
    /// in the table: the dropdown still completes, an empty box still walks
    /// the cards, and with no turn running there is nothing to wait for.
    #[test]
    fn tab_completes_before_it_queues_and_queues_only_while_a_turn_runs() {
        let (mut ui, now) = scene();
        ui.catalogs.commands = crate::commands::entries(vec![bingo_sdk::CommandSpec {
            opens: false,
            keys: vec![],
            hidden: false,
            name: "compact".into(),
            aliases: vec![],
            hint: String::new(),
            args: bingo_sdk::ArgSpec::None,
            instant: false,
            family: "kernel".into(),
        }]);
        write(&mut ui, &busy(), "/comp", now);
        assert!(
            press(&mut ui, &busy(), key(KeyCode::Tab), now).is_empty(),
            "the dropdown is what tab is for while one is open"
        );
        assert_eq!(
            ui.composer.text(),
            "/comp",
            "fallback leaves explicit typing alone"
        );

        let (mut ui, now) = scene();
        assert!(
            press(&mut ui, &busy(), key(KeyCode::Tab), now).is_empty(),
            "an empty box queues nothing"
        );
        write(&mut ui, &state(), "no turn is running", now);
        assert!(
            press(&mut ui, &state(), key(KeyCode::Tab), now).is_empty(),
            "and with nothing running there is nothing to wait for"
        );
        assert_eq!(ui.composer.text(), "no turn is running", "nor is it sent");
    }

    /// The user's first ask: a queued line comes back to be edited. `↑` walks
    /// into the person's own entries, newest first, and asks for that one.
    #[test]
    fn up_on_an_empty_box_asks_for_the_newest_line_of_ours_that_is_queued() {
        let (mut ui, now) = scene();
        let state = queued(&[("tui", "first"), ("tui", "second")]);
        assert_eq!(
            press(&mut ui, &state, key(KeyCode::Up), now),
            vec![Effect::Withdraw(IntentId::from_raw("req_1"))],
            "the newest, which is the last one queued"
        );
        assert!(
            ui.composer.is_empty(),
            "the answer fills the box, not the key"
        );
    }

    /// Only ours: another surface's queued line is not this one's to take
    /// back, so `↑` walks past it into the prompt history.
    #[test]
    fn up_walks_past_another_surfaces_queued_line_into_the_history() {
        let (mut ui, now) = scene();
        ui.history = crate::history::PromptHistory::new(vec!["an older ask".into()]);
        let state = queued(&[("rpc", "not ours")]);
        assert!(press(&mut ui, &state, key(KeyCode::Up), now).is_empty());
        assert_eq!(ui.composer.text(), "an older ask");
    }

    /// And with nothing of the person's waiting, `↑` is the history key it
    /// has always been.
    #[test]
    fn up_with_an_empty_queue_recalls_the_prompt_history() {
        let (mut ui, now) = scene();
        ui.history = crate::history::PromptHistory::new(vec!["an older ask".into()]);
        assert!(press(&mut ui, &busy(), key(KeyCode::Up), now).is_empty());
        assert_eq!(ui.composer.text(), "an older ask");
    }
}

#[cfg(test)]
mod m148 {
    use crate::test_support::*;
    use bingo_sdk::{Action, ActionItem, Event, Input, ItemId, ItemStatus, slot::Slot};
    use crossterm::event::KeyCode;

    fn with_actions() -> bingo_sdk::SessionState {
        crate::transcript::slots::fixtures::with_row(
            folded(vec![frame(
                1,
                Event::ItemCompleted {
                    item: assistant("itm_board", "old text", ItemStatus::Completed),
                },
            )]),
            Slot::Row {
                item: ItemId::from_raw("itm_board"),
                mark: None,
                title: Some("Board".into()),
                badge: None,
                caption: None,
                body: Some(bingo_sdk::View::text("table")),
                actions: vec![ActionItem {
                    label: "Tick".into(),
                    key: None,
                    action: Action {
                        name: "stranger.tick".into(),
                        args: serde_json::Value::Null,
                    },
                }],
            },
        )
    }

    #[test]
    fn m148_tab_focuses_a_placed_row_and_its_button_dispatches() {
        let state = with_actions();
        let (mut ui, now) = scene();
        render(&state, &mut ui, now);
        super::on_key(&mut ui, &solo(&state), key(KeyCode::Tab), now);
        assert_eq!(ui.focus.row(), Some(&ItemId::from_raw("itm_board")));
        assert!(render(&state, &mut ui, now).contains("❯ Board"));
        let effects = super::on_key(&mut ui, &solo(&state), typed('1'), now);
        assert!(
            matches!(&effects[..], [crate::effect::Effect::Submit(Input::Action { action, .. })] if action.name == "stranger.tick")
        );
        assert!(ui.composer.is_empty());
        super::on_key(&mut ui, &solo(&state), key(KeyCode::Tab), now);
        assert!(ui.focus.row().is_none());
        super::on_key(&mut ui, &solo(&state), typed('1'), now);
        assert_eq!(ui.composer.text(), "1");
    }

    #[test]
    fn m148_row_focus_uses_the_ascii_cursor() {
        crate::theme::with(
            crate::theme::Theme {
                glyphs: &crate::theme::ASCII,
                ..crate::render::Render::default().look
            },
            || {
                let state = with_actions();
                let (mut ui, now) = scene();
                super::on_key(&mut ui, &solo(&state), key(KeyCode::Tab), now);
                let screen = render(&state, &mut ui, now);
                assert!(screen.contains("> Board"), "{screen}");
                assert!(!screen.contains('❯'));
            },
        );
    }
}

pub(crate) fn palette_edit(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    editing(ui, tree, key, now)
}
pub(crate) fn palette_submit(ui: &mut Ui, tree: &Tree, now: Now) -> Vec<Effect> {
    submit(ui, tree, now, Delivery::default())
}

#[cfg(test)]
mod paste_focus_tests {
    #[test]
    fn paste_waiting_for_a_panes_editor_cannot_spill_into_the_composer() {
        let (mut ui, _) = crate::test_support::scene();
        ui.focus = crate::focus::Focus::Pane("picker".into());
        super::on_paste(&mut ui, "sk-dummy-never-unmasked");
        assert!(ui.composer.is_empty());
    }
}

#[cfg(test)]
mod return_tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn escape_returns_only_after_overlays_and_work_and_keeps_a_draft() {
        let mut tree = room_tree(vec![]);
        let (mut ui, now) = scene();
        ui.composer.set("a draft");
        assert!(on_key(&mut ui, &tree, key(KeyCode::Esc), now).is_empty());
        ui.composer.clear();
        crate::palette::open(&mut ui, bingo_sdk::component::PaletteOpen::Sessions, now);
        assert!(on_key(&mut ui, &tree, key(KeyCode::Esc), now).is_empty());
        assert_eq!(
            on_key(&mut ui, &tree, key(KeyCode::Esc), now),
            [Effect::View(tree.root_id().clone())]
        );
        tree.fold.apply(&log_frame(2, started("trn_room")));
        assert_eq!(
            on_key(&mut ui, &tree, key(KeyCode::Esc), now),
            [Effect::Interrupt]
        );
    }
}
