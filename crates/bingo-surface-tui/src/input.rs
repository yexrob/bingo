//! One pure function from a key to a list of effects. It mutates the surface's
//! own `Ui` and reads the folded `SessionState`; it calls nothing, so a key
//! table is a test with no terminal and no kernel in it.
//!
//! Only lines that reach the kernel are appended to the history file: the loop
//! writes what it submits, and a surface-local command never gets that far.

use std::path::PathBuf;

use bingo_sdk::{Input, Level, Origin, SessionId, SessionSelector, SessionSpec, SessionState};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::SURFACE_ID;
use crate::clock::Now;
use crate::commands::{self, Local};
use crate::effect::Effect;
use crate::fold;
use crate::keys;
use crate::mentions;
use crate::pager;
use crate::rail::{self, CardId, Pin};
use crate::rewind::{self, Rewind};
use crate::search::Search;
use crate::tree::Tree;
use crate::ui::{Open, Pending, Ui};
use crate::{panel, permission, views};

mod switcher;

pub(crate) use switcher::walk_to;

/// What the first ctrl+c on an empty composer says.
pub const ARM_HINT: &str = "press ctrl+c again to exit";
/// What shift+tab says when no policy published a mode it can walk.
pub const UNKNOWN_MODE: &str = "permission mode unknown — /permission <mode>";
/// What ctrl+b says when no shell command is running to background.
pub const NOTHING_RUNNING: &str = "no shell command is running";
/// The command the shell plugin registers for backgrounding a running one
/// (ADR-0018 §6). A surface may not import a plugin (ADR-0001), so the name is
/// the whole of the contract between them.
const PROMOTE: &str = "bash.promote";

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
    // A prompt raised anywhere in the tree is answered from wherever the
    // person is looking; the handle routes the answer back to who asked. It
    // comes before the chords a card has a use of its own for — a form's
    // `tab` walks its questions (M53) — as every layer that captures does.
    if let Some((_, interaction)) = tree.open_interaction() {
        return ui.dialog.on_key(interaction, key, now);
    }
    if key.code == KeyCode::Tab && suggestions(ui, tree).is_empty() && cycle_focus(ui, tree) {
        return Vec::new();
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
        // A chord is never the list's own: `ctrl+g` closes it below, and every
        // other opens something else over it. Either way the list goes, so the
        // draft it set aside comes back to the box first (M58) — otherwise the
        // query a person typed would be left there for `⏎` to send.
        if chord != switcher::CHORD {
            switcher::put_away(ui, now);
        }
        match chord {
            'f' => ui.search = Some(Search::open()),
            switcher::CHORD => return Some(switcher::toggle(ui, tree, now)),
            't' => ui.layer.toggle(Open::Panel, now.instant),
            'o' => deepen(ui, tree, now),
            'b' => return Some(background(ui, tree, now)),
            _ => return None,
        }
        return Some(Vec::new());
    }
    if !ui.layer.captures() {
        return None;
    }
    Some(match ui.layer.open {
        Open::Panel => panel_keys(ui, tree, key, now),
        _ => switcher::keys(ui, tree, key, now),
    })
}

/// The letter of a control chord, if that is what this key is.
fn chorded(key: KeyEvent) -> Option<char> {
    match (key.code, key.modifiers.contains(KeyModifiers::CONTROL)) {
        (KeyCode::Char(c), true) => Some(c),
        _ => None,
    }
}

/// `ctrl+o` only ever opens further: the first press lifts the fold on the
/// focused result — else the latest — and the second takes the whole of it into
/// the pager, where `esc` folds it again. One key, one direction (§4's
/// `ctrl+o to expand`).
fn deepen(ui: &mut Ui, tree: &Tree, now: Now) {
    let focused = ui.select.block.clone();
    let Some(id) = latest(tree.viewed(), focused.as_ref(), folds) else {
        return;
    };
    let Some(item) = item_of(tree.viewed(), &id) else {
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
/// id, and the rail draws the job from the signal it already publishes.
fn background(ui: &mut Ui, tree: &Tree, now: Now) -> Vec<Effect> {
    let Some(call) = running_shell(tree.viewed()) else {
        ui.notify(Level::Info, NOTHING_RUNNING, now.instant);
        return Vec::new();
    };
    vec![Effect::Submit(Input::Action {
        action: bingo_sdk::Action {
            name: PROMOTE.into(),
            args: serde_json::Value::String(call),
        },
    })]
}

/// The call id of the shell command in flight, the newest first.
fn running_shell(state: &SessionState) -> Option<String> {
    state
        .items
        .iter()
        .rev()
        .filter(|item| !item.is_terminal())
        .find_map(|item| match &item.body {
            bingo_sdk::ItemBody::ToolCall { call_id, name, .. } if name == "Bash" => {
                Some(call_id.clone())
            }
            _ => None,
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
        .items
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
    state.items.iter().find(|item| &item.id == id)
}

/// What `ctrl+o` opens further: a block whose row wears
/// `… +N lines (ctrl+o to expand)` — a call that came back, a thought that was
/// not redacted, an action's own result. A quiet notice is deliberately not
/// one: its cut promises no key (M11's rule, 2026-09-01), and a click is what
/// opens it.
fn folds(item: &bingo_sdk::Item) -> bool {
    match &item.body {
        bingo_sdk::ItemBody::ToolCall { output, .. } => output.is_some(),
        // Only once the thinking is over, as a call folds only once it has
        // come back: while it is being had the row wears the same three tail
        // rows a running tool does, which cut nothing and promise no key.
        bingo_sdk::ItemBody::Reasoning { .. } => {
            item.completed_at.is_some() && crate::transcript::thought(item).is_some()
        }
        bingo_sdk::ItemBody::Action { result, .. } => result.is_some(),
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

/// What the dropdown is offering for the line being typed: the one question
/// every key that the dropdown owns asks, over the session on the screen.
fn suggestions(ui: &Ui, tree: &Tree) -> Vec<commands::Suggestion> {
    ui.suggestions(cwd(tree), &mentions::targets(tree))
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
        keys::Interrupt::Turn => return ask_to_stop(ui, state),
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
        dropdown: !suggestions(ui, tree).is_empty(),
        busy: tree.viewed().busy(),
    };
    // An `esc` that closed something is not half of a gesture.
    let rung = keys::escape(open);
    ui.esc_armed &= rung.is_none();
    match rung {
        Some(keys::Escape::Sheet) => ui.layer.close(now.instant),
        Some(keys::Escape::Card) => return cancel(ui, tree, now),
        Some(keys::Escape::Dropdown) => ui.menu.dismissed = true,
        Some(keys::Escape::Interrupt) => return ask_to_stop(ui, tree.viewed()),
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

/// The dropdown owns the arrows and the completion keys while it is open.
fn menu(ui: &mut Ui, tree: &Tree, key: KeyEvent) -> Option<Vec<Effect>> {
    let rows = suggestions(ui, tree);
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
        // The list's other door: `↓` on an empty box opens the same list
        // `ctrl+g` does, with the cursor on the session already in view.
        KeyCode::Down if switcher::opens(ui, tree) => return switcher::toggle(ui, tree, now),
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
    // The gesture is answered on its own frame, whatever the line turns out
    // to be and whatever the kernel makes of it (§6).
    ui.sent = Some(now.instant);
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
        // The pasted pictures the line still names go beside it; the ones it
        // mentions by path are read by the loop, which knows the directory.
        None => vec![Effect::Submit(Input::Text {
            images: ui.pictures.carried(&text),
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
    use crate::pointer::{WHEEL, on_mouse};
    use crate::roster;
    use crate::test_support::*;
    use bingo_sdk::{Activation, Answer, SessionId, TurnId, TurnStatus};
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
                origin: Origin::surface(SURFACE_ID),
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

    /// The tokens still in the line at `⏎` say which held pictures go, in
    /// the line's order; a deleted token's picture stays behind.
    #[test]
    fn a_pasted_picture_goes_beside_the_line_that_still_names_it() {
        let (mut ui, now) = scene();
        let tree = solo(&state());
        let first = bingo_sdk::Image::from_bytes("image/png", b"one").unwrap();
        let second = bingo_sdk::Image::from_bytes("image/png", b"two").unwrap();
        let n = ui.pictures.hold("", first);
        ui.composer
            .insert(&format!("see {} ", crate::pictures::placeholder(n)));
        let n = ui.pictures.hold(ui.composer.text(), second.clone());
        ui.composer.insert(&crate::pictures::placeholder(n));
        ui.composer.set("see [image 2]");
        let effects = on_key(&mut ui, &tree, key(KeyCode::Enter), now);
        assert_eq!(
            effects,
            vec![Effect::Submit(Input::Text {
                text: "see [image 2]".into(),
                images: vec![second],
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

    /// Where the list's cursor is, when it is the layer that is open.
    fn selected(ui: &Ui) -> Option<roster::Cursor> {
        match &ui.layer.open {
            Open::Switcher(switcher) => Some(switcher.cursor),
            _ => None,
        }
    }

    /// A row of the list, by its number.
    fn at(index: usize) -> Option<roster::Cursor> {
        Some(roster::Cursor { at: index })
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
        for key in [key(KeyCode::Enter), key(KeyCode::Enter), typed(' ')] {
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
                            ids: vec!["0".into()]
                        },
                        Answer::Choice {
                            ids: vec!["0".into()]
                        },
                        Answer::Choice {
                            ids: vec!["0".into()]
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
        assert_eq!(
            press_tree(&mut ui, &tree, ctrl('g'), now),
            vec![Effect::ListStored],
            "the tree is on the card at once and the store is asked for the rest"
        );
        assert_eq!(selected(&ui), at(0), "it opens on the session in view");
        assert_eq!(
            walked_to(&mut ui, &tree, key(KeyCode::Down), now),
            Some(child_id()),
            "the walk is the switch"
        );
        assert!(press_tree(&mut ui, &tree, key(KeyCode::Enter), now).is_empty());
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

    /// A root alone in this process may still have children in the store, and
    /// only the store knows: the card goes up and the read fills it.
    #[test]
    fn ctrl_g_on_a_lone_root_still_asks_what_the_store_holds() {
        let (mut ui, now) = scene();
        assert_eq!(
            press(&mut ui, &state(), ctrl('g'), now),
            vec![Effect::ListStored]
        );
        assert!(ui.layer.showing());
        assert!(ui.notices.is_empty(), "nothing is said about an empty tree");
    }

    /// The stored rows the read answered with: chosen by `⏎`, they are
    /// reopened by id, and the loop's `View` is what does it.
    #[test]
    fn enter_on_a_stored_row_steps_into_the_session_it_names() {
        let tree = with_child(vec![]);
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        let Open::Switcher(open) = &mut ui.layer.open else {
            panic!("the switcher is open");
        };
        open.stored = vec![stored_summary("ses_7", "scout")];
        for _ in 0..2 {
            press_tree(&mut ui, &tree, key(KeyCode::Down), now);
        }
        assert_eq!(selected(&ui), at(2), "the stored row is walked to");
        assert!(
            press_tree(&mut ui, &tree, key(KeyCode::Enter), now).is_empty(),
            "the walk already showed it; `⏎` only settles on where it landed"
        );
        assert!(!ui.layer.showing());
    }

    /// The session a walk switched the view to, when it switched it.
    fn walked_to(
        ui: &mut Ui,
        tree: &Tree,
        key: crossterm::event::KeyEvent,
        now: Now,
    ) -> Option<SessionId> {
        match press_tree(ui, tree, key, now).first() {
            Some(Effect::View(id)) => Some(id.clone()),
            _ => None,
        }
    }

    #[test]
    fn the_switcher_opens_on_the_child_that_is_already_in_view() {
        let mut tree = with_child(vec![]);
        tree.show(&child_id());
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        assert_eq!(selected(&ui), at(1));
        assert!(
            press_tree(&mut ui, &tree, key(KeyCode::Enter), now).is_empty(),
            "it is already the session on screen, so settling on it asks for nothing"
        );
    }

    // ---- the list is typed into (M55, re-cut by M58) ---------------------

    /// A root with two sub-agents under it, so a query has rows to leave out:
    /// `project`, `reviewer`, `scout`.
    fn with_agents() -> Tree {
        folded_tree(vec![
            child_frame(1, announced("reviewer")),
            agent_frame(3, 2, agent_announced(3, "scout")),
        ])
    }

    /// The line the list set aside when it went up, if it is up.
    fn draft(ui: &Ui) -> String {
        match &ui.layer.open {
            Open::Switcher(switcher) => switcher.draft.clone(),
            _ => String::new(),
        }
    }

    fn typing(ui: &mut Ui, tree: &Tree, text: &str, now: Now) -> Vec<Effect> {
        let mut effects = Vec::new();
        for c in text.chars() {
            effects = press_tree(ui, tree, key(KeyCode::Char(c)), now);
        }
        effects
    }

    /// The ask M55 answers, in the shape M58 gave it: what is typed lands in
    /// the **input box**, the list narrows on it, the cursor lands on what is
    /// left, and the view follows the cursor as it does on a walk — so `⏎`
    /// keeps what a person is looking at.
    #[test]
    fn a_typed_query_narrows_the_list_and_the_view_follows_it() {
        let tree = with_agents();
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        assert_eq!(selected(&ui), at(0), "it opens on the session in view");
        let effects = typing(&mut ui, &tree, "sco", now);
        assert_eq!(ui.composer.text(), "sco", "the query is the box's line");
        assert_eq!(selected(&ui), at(0), "the one row left is the first row");
        assert_eq!(
            effects,
            vec![Effect::View(agent_id(3))],
            "and the row the cursor landed on is the view"
        );
    }

    /// `esc` is one ordered stack (§7): the query a person typed is the first
    /// thing it takes back, and the list itself the next.
    #[test]
    fn esc_takes_the_query_back_before_it_closes_the_list() {
        let mut tree = with_agents();
        let root = tree.root_id().clone();
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        typing(&mut ui, &tree, "sco", now);
        // The loop applied the `View` the narrowing asked for.
        tree.show(&agent_id(3));

        let effects = press_tree(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(ui.layer.showing(), "the first `esc` is the query's");
        assert!(ui.composer.is_empty(), "and it takes the box's line back");
        assert_eq!(
            selected(&ui),
            at(2),
            "and the cursor keeps the session it was on"
        );
        assert!(effects.is_empty(), "nothing moved, so nothing is asked for");

        let effects = press_tree(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(!ui.layer.showing(), "the second closes the list");
        assert_eq!(
            effects,
            vec![Effect::View(root)],
            "and gives back the session it was opened from"
        );
    }

    /// Backspace gives the rows back a letter at a time, and one on an empty
    /// box is the box's own no-op — it never reaches the `esc` stack.
    #[test]
    fn backspace_gives_the_rows_back_letter_by_letter() {
        let mut tree = with_agents();
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        typing(&mut ui, &tree, "sco", now);
        tree.show(&agent_id(3));
        for _ in 0..3 {
            press_tree(&mut ui, &tree, key(KeyCode::Backspace), now);
        }
        assert!(ui.composer.is_empty());
        assert_eq!(selected(&ui), at(2), "still on the row it narrowed to");
        press_tree(&mut ui, &tree, key(KeyCode::Backspace), now);
        assert!(ui.layer.showing(), "and the list is still up");
    }

    /// A second opening starts with nothing typed: the query is a fact about
    /// this gesture, like where it started from.
    #[test]
    fn the_list_opens_with_nothing_typed_into_it() {
        let tree = with_agents();
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        typing(&mut ui, &tree, "sco", now);
        press_tree(&mut ui, &tree, ctrl('g'), now);
        press_tree(&mut ui, &tree, ctrl('g'), now);
        assert!(ui.composer.is_empty());
    }

    /// M58's own ask: the box is the query line, so the line a person was
    /// writing is set aside while the list is up and is back in the box —
    /// caret at its end — the moment the list goes, whichever key took it.
    #[test]
    fn the_line_being_written_is_set_aside_and_given_back() {
        let tree = with_agents();
        // `esc` is a stack: the first press is the query's, the second the
        // list's (§7).
        for (closing, presses) in [
            (key(KeyCode::Enter), 1),
            (key(KeyCode::Esc), 2),
            (ctrl('g'), 1),
        ] {
            let (mut ui, now) = scene();
            write(&mut ui, tree.viewed(), "half a thought", now);
            press_tree(&mut ui, &tree, ctrl('g'), now);
            assert!(ui.composer.is_empty(), "the box opens the list empty");
            assert_eq!(draft(&ui), "half a thought", "and the line is kept");

            typing(&mut ui, &tree, "sco", now);
            assert_eq!(ui.composer.text(), "sco");

            for _ in 0..presses {
                press_tree(&mut ui, &tree, closing, now);
            }
            assert!(!ui.layer.showing());
            assert_eq!(
                ui.composer.text(),
                "half a thought",
                "the draft is back exactly as it was"
            );
        }
    }

    /// A chord is never the list's own, so it takes the list away and gives
    /// the draft back before it opens what it opens — a query left in the box
    /// would be a line `⏎` sends.
    #[test]
    fn a_chord_that_opens_something_else_puts_the_list_away() {
        let tree = with_agents();
        let (mut ui, now) = scene();
        write(&mut ui, tree.viewed(), "half a thought", now);
        press_tree(&mut ui, &tree, ctrl('g'), now);
        typing(&mut ui, &tree, "sco", now);
        press_tree(&mut ui, &tree, ctrl('t'), now);
        assert!(
            ui.layer.is(&Open::Panel),
            "the chord's own layer is what is up"
        );
        assert_eq!(ui.composer.text(), "half a thought");
    }

    /// The box is the list's line while the list is up: nothing it holds
    /// offers a command or a mention, so no second dropdown is drawn over the
    /// one that is open.
    #[test]
    fn the_query_offers_no_command_of_its_own() {
        let tree = with_agents();
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        typing(&mut ui, &tree, "/mo", now);
        assert_eq!(ui.composer.text(), "/mo");
        assert!(
            ui.suggestions(&tree.viewed().summary.cwd, &[]).is_empty(),
            "the list owns the line"
        );
    }

    // ---- `tab` completes (M58) ------------------------------------------

    /// `tab` completes the name the cursor is on into the box, the way `tab`
    /// on the `/` dropdown completes a command; the list then holds the one
    /// row the completed name leaves.
    #[test]
    fn tab_completes_the_name_the_cursor_is_on() {
        let mut tree = with_agents();
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        assert_eq!(
            walked_to(&mut ui, &tree, key(KeyCode::Down), now),
            Some(child_id()),
        );
        // The loop applied the `View` the walk asked for.
        tree.show(&child_id());

        let effects = press_tree(&mut ui, &tree, key(KeyCode::Tab), now);
        assert_eq!(ui.composer.text(), "reviewer", "the name is in the box");
        assert_eq!(
            selected(&ui),
            at(0),
            "and the list is the one row it narrowed to"
        );
        assert!(
            effects.is_empty(),
            "the walk already showed it, so nothing is asked for"
        );
        assert!(
            press_tree(&mut ui, &tree, key(KeyCode::Enter), now).is_empty(),
            "`⏎` keeps the session the completed name named"
        );
        assert!(!ui.layer.showing());
        assert!(ui.composer.is_empty(), "and the box is the draft's again");
    }

    /// A room completes as its own `#name`: the sigil is part of what it is
    /// called, and it is by that name the list finds it again.
    #[test]
    fn tab_completes_a_rooms_name_with_its_sigil() {
        let tree = folded_tree(vec![
            child_frame(1, announced("reviewer")),
            log_frame(2, log_announced("#design")),
        ]);
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        typing(&mut ui, &tree, "des", now);
        press_tree(&mut ui, &tree, key(KeyCode::Tab), now);
        assert_eq!(ui.composer.text(), "#design");
        assert_eq!(selected(&ui), at(0));
    }

    /// A list with nothing left on it has no name to complete, and `tab`
    /// leaves the line a person is typing exactly as it is.
    #[test]
    fn tab_completes_nothing_where_the_query_left_no_row() {
        let tree = with_agents();
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        typing(&mut ui, &tree, "zzz", now);
        press_tree(&mut ui, &tree, key(KeyCode::Tab), now);
        assert_eq!(ui.composer.text(), "zzz");
        assert!(ui.layer.showing(), "and the list is still up");
    }

    // ---- one list, two doors --------------------------------------------

    /// `↓` means two things and a query changes neither: on an empty composer
    /// it opens the list, and inside it it walks — the rows the query left.
    #[test]
    fn down_opens_the_list_and_then_walks_what_the_query_left() {
        let tree = with_agents();
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, key(KeyCode::Down), now);
        // `o` is in `project` and in `scout` and in neither's way.
        typing(&mut ui, &tree, "o", now);
        assert_eq!(selected(&ui), at(0), "the root is still the row in view");
        assert_eq!(
            walked_to(&mut ui, &tree, key(KeyCode::Down), now),
            Some(agent_id(3)),
            "and the step goes to the next row the query left, not the tree's"
        );
        assert_eq!(selected(&ui), at(1));
    }

    /// `↓` on an empty composer opens the same list `ctrl+g` does, and asks
    /// the store the same question: they are one gesture with two keys, so
    /// what they leave behind is one state.
    #[test]
    fn down_on_an_empty_line_opens_the_list_ctrl_g_opens() {
        let tree = with_child(vec![]);
        let (mut down, now) = scene();
        let (mut chord, _) = scene();
        assert_eq!(
            press_tree(&mut down, &tree, key(KeyCode::Down), now),
            vec![Effect::ListStored]
        );
        press_tree(&mut chord, &tree, ctrl('g'), now);
        assert_eq!(down.layer.open, chord.layer.open);
        assert!(down.layer.showing());
    }

    /// Walking is switching: the transcript changes as the cursor moves, as
    /// the strip's walk did (§3).
    #[test]
    fn walking_the_list_switches_the_view_as_the_cursor_moves() {
        let tree = with_child(vec![]);
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, key(KeyCode::Down), now);
        assert_eq!(
            walked_to(&mut ui, &tree, key(KeyCode::Down), now),
            Some(child_id()),
            "the row below the root is the child, and the view goes with it"
        );
        assert_eq!(selected(&ui), at(1));
        assert!(ui.layer.showing(), "and the list stays up to be walked on");
    }

    /// The column is walked to its ends and stops there: a list is not a ring,
    /// and there is nothing past either end to wrap round to.
    #[test]
    fn the_column_stops_at_its_ends() {
        let tree = with_child(vec![]);
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        press_tree(&mut ui, &tree, key(KeyCode::Up), now);
        assert_eq!(selected(&ui), at(0), "the first row is the first row");
        for _ in 0..4 {
            press_tree(&mut ui, &tree, key(KeyCode::Down), now);
        }
        assert_eq!(selected(&ui), at(1), "and the last is the last");
    }

    /// `esc` puts back the session the list was opened from, however far the
    /// walk went; `⏎` keeps the one it landed on.
    #[test]
    fn esc_gives_back_where_the_walk_started_and_enter_keeps_where_it_ended() {
        let tree = with_child(vec![]);
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        walked_to(&mut ui, &tree, key(KeyCode::Down), now);
        assert_eq!(
            walked_to(&mut ui, &tree, key(KeyCode::Esc), now),
            Some(tree.root_id().clone()),
            "back to the session the list was opened from"
        );
        assert!(!ui.layer.showing());

        press_tree(&mut ui, &tree, ctrl('g'), now);
        walked_to(&mut ui, &tree, key(KeyCode::Down), now);
        assert!(
            press_tree(&mut ui, &tree, key(KeyCode::Enter), now).is_empty(),
            "the walked-to session is already on screen"
        );
        assert!(!ui.layer.showing());
    }

    /// `esc` on the list closes the list and nothing else: it is the one thing
    /// a person opened, so it is the one thing that press takes away.
    #[test]
    fn esc_on_the_list_does_not_reach_the_turn() {
        let tree = with_child(vec![frame(2, started("trn_1"))]);
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, key(KeyCode::Down), now);
        press_tree(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(!ui.layer.showing());
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
        assert!(!ui.layer.showing(), "and a session alone opened no list");
    }

    #[test]
    fn a_half_typed_line_keeps_the_arrows_for_itself() {
        let tree = with_child(vec![]);
        let (mut ui, now) = scene();
        write(&mut ui, tree.viewed(), "half a thought", now);
        press_tree(&mut ui, &tree, key(KeyCode::Down), now);
        assert!(!ui.layer.showing());
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
        // Row 17 is the transcript's last: the reserved activity band holds
        // the two rows beneath it (view.rs's demand).
        on_mouse(&mut ui, &solo(&state), click(4, 17), now);
        assert_eq!(
            ui.select.block,
            Some(bingo_sdk::ItemId::from_raw("itm_59")),
            "the last row is the last item"
        );
        assert!(ui.select.run.is_some(), "and a run starts there");
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
        ui.painted.borrow().regions.transcript.bottom() - 1
    }

    /// A click on a fold is one gesture that opens it and folds it again — the
    /// set `ctrl+o` fills, reached from the mouse (design §7).
    #[test]
    fn a_click_opens_a_fold_and_a_second_click_folds_it() {
        let state = long_result();
        let tree = solo(&state);
        let (mut ui, now) = scene();
        assert!(render(&state, &ui, now).contains("+4 lines (ctrl+o to expand)"));

        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        let opened = render(&state, &ui, now);
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
            render(&state, &ui, now).contains("+4 lines (ctrl+o to expand)"),
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
        assert!(render(&state, &ui, now).contains("+4 lines"));
        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        let opened = render(&state, &ui, now);
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

    /// `ctrl+o` only ever opens further (§7), and a thought has one rung more
    /// than a result because it starts one lower: shut, its first two rows,
    /// the whole of it, the sheet. `esc` out of the sheet puts it back where
    /// its kind starts.
    #[test]
    fn ctrl_o_climbs_a_thought_from_shut_through_peek_and_whole_to_the_sheet() {
        let state = thought(&steps());
        let (mut ui, now) = scene();
        let shut = render(&state, &ui, now);
        assert!(shut.contains("Thought for 2s"), "{shut}");
        assert!(!shut.contains("step 1"), "it starts shut: {shut}");

        press(&mut ui, &state, ctrl('o'), now);
        let peek = render(&state, &ui, now);
        assert!(peek.contains("step 2"), "{peek}");
        assert!(!peek.contains("step 3"), "two rows, from the top: {peek}");
        assert!(peek.contains("+7 lines (ctrl+o to expand)"), "{peek}");

        press(&mut ui, &state, ctrl('o'), now);
        let opened = render(&state, &ui, now);
        assert!(opened.contains("step 9"), "{opened}");

        press(&mut ui, &state, ctrl('o'), now);
        assert!(
            render(&state, &ui, later(now, 200)).contains("Thinking"),
            "the sheet says what it is"
        );

        press(&mut ui, &state, key(KeyCode::Esc), now);
        assert!(ui.folds.is_empty(), "and esc folds it back to shut");
    }

    /// A click walks the same rungs and comes back round (§7): a thought that
    /// is over is the one block with three states, because it is the one with
    /// a state a person wants to skip.
    #[test]
    fn a_click_cycles_a_finished_thought_through_its_three_states() {
        let state = thought(&steps());
        let tree = solo(&state);
        let (mut ui, now) = scene();
        assert!(!render(&state, &ui, now).contains("step 1"), "shut");

        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        let peek = render(&state, &ui, now);
        assert!(peek.contains("step 1") && peek.contains("step 2"), "{peek}");
        assert!(!peek.contains("step 3"), "the peek is two rows: {peek}");

        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        let opened = render(&state, &ui, now);
        assert!(opened.contains("step 9"), "{opened}");
        assert!(!opened.contains("+7 lines"), "{opened}");

        let row = last_row(&ui);
        on_mouse(&mut ui, &tree, click(6, row), now);
        assert!(
            !render(&state, &ui, now).contains("step 1"),
            "the third click is back where the first started"
        );
    }

    /// A redacted thought promises nothing: no fold, no key, no sheet.
    #[test]
    fn an_empty_thought_opens_nothing() {
        let state = thought("");
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        press(&mut ui, &state, ctrl('o'), now);
        assert!(ui.folds.is_empty(), "there is nothing to lift");
        ui.select.block = Some(bingo_sdk::ItemId::from_raw("itm_1"));
        press(&mut ui, &state, key(KeyCode::Enter), now);
        assert!(!ui.layer.showing(), "and nothing to open");
    }

    /// A thought being had is not a fold: its two tail rows scroll rather than
    /// cut, and promise no key, so `ctrl+o` walks past it to the thought that
    /// is over — the same rule a running call keeps.
    #[test]
    fn ctrl_o_walks_past_a_thought_that_is_still_being_had() {
        let mut state = thought(&steps());
        let being_had = crate::test_support::item(
            "itm_2",
            bingo_sdk::ItemStatus::Running,
            bingo_sdk::ItemBody::Reasoning {
                text: "and now the lockfile".into(),
                provider_metadata: Default::default(),
            },
        );
        state.apply(&frame(2, bingo_sdk::Event::ItemStarted { item: being_had }));
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        press(&mut ui, &state, ctrl('o'), now);
        assert_eq!(
            ui.folds.keys().map(ToString::to_string).collect::<Vec<_>>(),
            vec!["itm_1".to_string()],
            "the thought that is over is the one with a fold to lift"
        );
    }

    /// `⏎` on a focused block still opens the pager, thought or result alike.
    #[test]
    fn enter_on_a_focused_thought_opens_the_sheet() {
        let state = thought("the manifest first");
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        ui.select.block = Some(bingo_sdk::ItemId::from_raw("itm_1"));
        press(&mut ui, &state, key(KeyCode::Enter), now);
        assert!(matches!(ui.layer.open, Open::Pager(_)));
    }

    #[test]
    fn a_drag_takes_the_far_end_of_the_run_with_it() {
        let state = long_transcript(60);
        let (mut ui, now) = scene();
        render(&state, &ui, now);
        on_mouse(&mut ui, &solo(&state), click(2, 15), now);
        on_mouse(&mut ui, &solo(&state), dragged(6, 17), now);
        let run = ui.select.run.expect("a run");
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
            [("run the tests", 2, 5, "run"), ("你好 warm", 2, 6, "你好")]
        {
            let state = folded(vec![frame(
                1,
                bingo_sdk::Event::ItemCompleted {
                    item: user("itm_1", text),
                },
            )]);
            let tree = solo(&state);
            let (mut ui, now) = scene();
            let row = row_carrying(&render(&state, &ui, now), text);
            on_mouse(&mut ui, &tree, click(from, row), now);
            on_mouse(&mut ui, &tree, dragged(to, row), now);
            assert_eq!(
                press(&mut ui, &state, typed('y'), now),
                vec![Effect::Copy(copied.to_string())],
                "dragging {from}..{to} over {text:?}"
            );
        }
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

    // ---- backgrounding the running command (ADR-0018 §6) ------------------

    /// A turn with a shell command in flight.
    fn running_bash() -> SessionState {
        folded(vec![
            frame(1, started("trn_1")),
            frame(
                2,
                bingo_sdk::Event::ItemStarted {
                    item: running_tool("itm_1", "Bash", "compiling…"),
                },
            ),
        ])
    }

    #[test]
    fn ctrl_b_fires_the_plugins_command_naming_the_call_that_is_running() {
        let (mut ui, now) = scene();
        let effects = press(&mut ui, &running_bash(), ctrl('b'), now);
        assert_eq!(
            effects,
            vec![Effect::Submit(Input::Action {
                action: bingo_sdk::Action {
                    name: "bash.promote".into(),
                    args: serde_json::Value::String("call_1".into()),
                },
            })]
        );
        assert!(ui.notices.is_empty(), "the plugin answers, not the surface");
    }

    #[test]
    fn ctrl_b_says_so_when_no_shell_command_is_running() {
        for state in [
            state(),
            busy(),
            // A finished call is not a running one.
            folded(vec![
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
            ]),
            // Another tool's call is not the shell's.
            folded(vec![
                frame(1, started("trn_1")),
                frame(
                    2,
                    bingo_sdk::Event::ItemStarted {
                        item: running_tool("itm_1", "Read", "reading…"),
                    },
                ),
            ]),
        ] {
            let (mut ui, now) = scene();
            assert!(press(&mut ui, &state, ctrl('b'), now).is_empty());
            assert!(ui.notices.iter().any(|n| n.text == NOTHING_RUNNING));
        }
    }

    /// The key is in the one binding table, so the `?` sheet prints it.
    #[test]
    fn ctrl_b_is_documented_where_every_key_is() {
        let row = keys::BINDINGS
            .iter()
            .find(|binding| binding.keys == "ctrl+b")
            .expect("a row for ctrl+b");
        assert_eq!(row.description, "background the running command");
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
