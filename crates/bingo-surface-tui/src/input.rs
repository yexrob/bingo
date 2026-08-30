//! One pure function from a key to a list of effects. It mutates the surface's
//! own `Ui` and reads the folded `SessionState`; it calls nothing, so a key
//! table is a test with no terminal and no kernel in it.
//!
//! Only lines that reach the kernel are appended to the history file: the loop
//! writes what it submits, and a surface-local command never gets that far.

use std::path::PathBuf;

use bingo_sdk::{Input, Level, Origin, SessionSelector, SessionSpec, SessionState};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::SURFACE_ID;
use crate::clock::Now;
use crate::commands::{self, Local};
use crate::effect::Effect;
use crate::permission;
use crate::tree::Tree;
use crate::ui::{Scroll, Switcher, Ui};

/// Lines the transcript moves by on one page key.
const PAGE: usize = 10;
/// What the first ctrl+c on an empty composer says.
pub const ARM_HINT: &str = "press ctrl+c again to exit";
/// What shift+tab says when no policy published a mode it can walk.
pub const UNKNOWN_MODE: &str = "permission mode unknown — /permission <mode>";
/// What ctrl+g says when the session has spawned nobody to switch to.
pub const NO_AGENTS: &str = "no sub-agents in this session";

pub fn on_key(ui: &mut Ui, tree: &Tree, key: KeyEvent, now: Now) -> Vec<Effect> {
    if key.kind == KeyEventKind::Release {
        return Vec::new();
    }
    ui.block = None;
    let state = tree.viewed();
    if let Some(effects) = leaving(ui, state, key, now) {
        return effects;
    }
    if ui.picker.is_some() {
        return picker(ui, key);
    }
    if chord(key, 'g') {
        toggle_switcher(ui, tree, now);
        return Vec::new();
    }
    if chord(key, 't') {
        ui.panel = !ui.panel;
        return Vec::new();
    }
    if ui.switcher.is_some() {
        return switcher(ui, tree, key);
    }
    // A prompt raised anywhere in the tree is answered from wherever the
    // person is looking; the handle routes the answer back to who asked.
    if let Some((_, interaction)) = tree.open_interaction() {
        return ui.dialog.on_key(interaction, key, now);
    }
    if key.code == KeyCode::Esc {
        return escape(ui, state);
    }
    if let Some(effects) = menu(ui, key) {
        return effects;
    }
    editing(ui, tree, key, now)
}

fn chord(key: KeyEvent, c: char) -> bool {
    key.code == KeyCode::Char(c) && key.modifiers.contains(KeyModifiers::CONTROL)
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

fn interrupt_or_exit(ui: &mut Ui, state: &SessionState, now: Now) -> Vec<Effect> {
    if state.busy() {
        return vec![Effect::Interrupt];
    }
    if !ui.composer.is_empty() {
        ui.composer.clear();
        ui.edited();
        return Vec::new();
    }
    if ui.exit_armed(now.instant) {
        return vec![Effect::Exit];
    }
    ui.armed = Some(now.instant);
    ui.notify(Level::Info, ARM_HINT, now.instant);
    Vec::new()
}

/// Esc closes the innermost thing that is open, then interrupts.
fn escape(ui: &mut Ui, state: &SessionState) -> Vec<Effect> {
    if ui.panel {
        ui.panel = false;
        return Vec::new();
    }
    if ui.help {
        ui.help = false;
        return Vec::new();
    }
    if !ui.suggestions().is_empty() {
        ui.menu.dismissed = true;
        return Vec::new();
    }
    if state.busy() {
        return vec![Effect::Interrupt];
    }
    Vec::new()
}

fn picker(ui: &mut Ui, key: KeyEvent) -> Vec<Effect> {
    let Some(picker) = ui.picker.as_mut() else {
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
        KeyCode::Esc => ui.picker = None,
        KeyCode::Enter => {
            let chosen = picker.sessions.get(picker.selected).map(|s| s.id.clone());
            ui.picker = None;
            if let Some(id) = chosen {
                return vec![Effect::Open(SessionSelector::ById { id })];
            }
        }
        _ => {}
    }
    Vec::new()
}

/// Open the switcher on the session in view, or close it again. There is
/// nothing to switch between until the session has spawned somebody.
fn toggle_switcher(ui: &mut Ui, tree: &Tree, now: Now) {
    if ui.switcher.take().is_some() {
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
    ui.switcher = Some(Switcher { selected });
}

fn switcher(ui: &mut Ui, tree: &Tree, key: KeyEvent) -> Vec<Effect> {
    let rows = tree.rows();
    let Some(mut selected) = ui.switcher.map(|s| s.selected) else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Up => selected = selected.saturating_sub(1),
        KeyCode::Down => selected = (selected + 1).min(rows.len().saturating_sub(1)),
        KeyCode::Esc => {
            ui.switcher = None;
            return Vec::new();
        }
        KeyCode::Enter => {
            ui.switcher = None;
            return rows
                .get(selected)
                .map(|row| vec![Effect::View(row.session.clone())])
                .unwrap_or_default();
        }
        _ => {}
    }
    ui.switcher = Some(Switcher { selected });
    Vec::new()
}

/// The dropdown owns the arrows and the completion keys while it is open.
fn menu(ui: &mut Ui, key: KeyEvent) -> Option<Vec<Effect>> {
    let rows = ui.suggestions();
    if rows.is_empty() {
        return (key.code == KeyCode::Tab).then(Vec::new);
    }
    match key.code {
        KeyCode::Up => ui.menu.selected = ui.menu.selected.saturating_sub(1),
        KeyCode::Down => ui.menu.selected = (ui.menu.selected + 1).min(rows.len() - 1),
        KeyCode::Tab => return Some(complete(ui)),
        // Enter completes only while there is something left to complete; a
        // name already typed in full is meant to run.
        KeyCode::Enter if adds_something(ui) => return Some(complete(ui)),
        KeyCode::Enter => {
            ui.menu.dismissed = true;
            return None;
        }
        _ => return None,
    }
    Some(Vec::new())
}

fn adds_something(ui: &Ui) -> bool {
    ui.selected_suggestion()
        .is_some_and(|chosen| chosen.value.trim_end() != ui.composer.text().trim_end())
}

fn complete(ui: &mut Ui) -> Vec<Effect> {
    if let Some(chosen) = ui.selected_suggestion() {
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
        KeyCode::Enter => return enter(ui, tree),
        KeyCode::BackTab => return cycle_mode(ui, tree.viewed(), now),
        KeyCode::Up => history_or_line(ui, Step::Up),
        KeyCode::Down => history_or_line(ui, Step::Down),
        KeyCode::PageUp => ui.scroll = Scroll(ui.scroll.0 + PAGE),
        KeyCode::PageDown => ui.scroll = Scroll(ui.scroll.0.saturating_sub(PAGE)),
        KeyCode::Left => ui.composer.left(),
        KeyCode::Right => ui.composer.right(),
        KeyCode::Home => ui.composer.home(),
        KeyCode::End => ui.composer.end(),
        KeyCode::Backspace => edit(ui, |c| c.backspace()),
        KeyCode::Delete => edit(ui, |c| c.delete()),
        KeyCode::Char('?') if ui.composer.is_empty() => ui.help = !ui.help,
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
fn enter(ui: &mut Ui, tree: &Tree) -> Vec<Effect> {
    if ui.composer.text().ends_with('\\') {
        ui.composer.backspace();
        newline(ui);
        return Vec::new();
    }
    if ui.composer.text().trim().is_empty() {
        return Vec::new();
    }
    submit(ui, tree)
}

/// What a line does. `/clear` starts a fresh session beside the root's, not
/// beside whichever child is on screen.
fn submit(ui: &mut Ui, tree: &Tree) -> Vec<Effect> {
    let text = ui.composer.take();
    ui.history.remember(&text);
    ui.edited();
    ui.scroll = Scroll::default();
    match commands::local(&text) {
        Some(Local::Help) => {
            ui.help = !ui.help;
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
        None => vec![Effect::Submit(Input::text(
            text,
            Origin::surface(SURFACE_ID),
        ))],
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
        assert!(ui.help);
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
        assert!(ui.notices.iter().any(|n| n.text == ARM_HINT));
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

    #[test]
    fn esc_closes_the_innermost_thing_then_interrupts() {
        let state = folded(vec![
            frame(1, started("trn_1")),
            frame(2, opened(permission(None, None))),
        ]);
        let (mut ui, now) = scene();
        ui.dialog.focus_on(state.interactions.first());
        ui.help = true;

        assert_eq!(
            press(&mut ui, &state, key(KeyCode::Esc), now),
            vec![Effect::Answer {
                interaction: state.interactions[0].id.clone(),
                answer: Answer::Deny { feedback: None },
                activation: Activation::Keyboard,
            }],
            "the dialog is first"
        );

        let busy = busy();
        assert!(press(&mut ui, &busy, key(KeyCode::Esc), now).is_empty());
        assert!(!ui.help, "then the help panel");

        write(&mut ui, &busy, "/he", now);
        assert!(press(&mut ui, &busy, key(KeyCode::Esc), now).is_empty());
        assert!(ui.menu.dismissed, "then the dropdown");

        assert_eq!(
            press(&mut ui, &busy, key(KeyCode::Esc), now),
            vec![Effect::Interrupt],
            "then the running turn"
        );
        assert!(
            press(
                &mut ui,
                &crate::test_support::state(),
                key(KeyCode::Esc),
                now
            )
            .is_empty(),
            "and then nothing"
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
        assert!(ui.help);
        press(&mut ui, &state(), typed('?'), now);
        assert!(!ui.help);
        write(&mut ui, &state(), "why", now);
        press(&mut ui, &state(), typed('?'), now);
        assert_eq!(ui.composer.text(), "why?");
        assert!(!ui.help);
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
        ui.picker = Some(crate::ui::Picker {
            sessions: vec![
                summary(),
                bingo_sdk::SessionSummary {
                    id: SessionId::from_raw("ses_2"),
                    ..summary()
                },
            ],
            selected: 0,
        });
        press(&mut ui, &state(), key(KeyCode::Down), now);
        assert_eq!(
            press(&mut ui, &state(), key(KeyCode::Enter), now),
            vec![Effect::Open(SessionSelector::ById {
                id: SessionId::from_raw("ses_2")
            })]
        );
        assert!(ui.picker.is_none());
    }

    // ---- the switcher ---------------------------------------------------

    #[test]
    fn ctrl_g_lists_the_tree_and_enter_switches_the_view() {
        let tree = with_child(vec![]);
        let (mut ui, now) = scene();
        assert!(press_tree(&mut ui, &tree, ctrl('g'), now).is_empty());
        assert_eq!(
            ui.switcher.map(|s| s.selected),
            Some(0),
            "it opens on the session in view"
        );
        press_tree(&mut ui, &tree, key(KeyCode::Down), now);
        assert_eq!(
            press_tree(&mut ui, &tree, key(KeyCode::Enter), now),
            vec![Effect::View(child_id())]
        );
        assert!(ui.switcher.is_none());
    }

    #[test]
    fn ctrl_g_toggles_and_esc_closes_the_switcher() {
        let tree = with_child(vec![]);
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        press_tree(&mut ui, &tree, ctrl('g'), now);
        assert!(ui.switcher.is_none(), "the same chord closes it");
        press_tree(&mut ui, &tree, ctrl('g'), now);
        press_tree(&mut ui, &tree, key(KeyCode::Esc), now);
        assert!(ui.switcher.is_none());
    }

    #[test]
    fn ctrl_g_says_so_when_there_is_nobody_to_switch_to() {
        let (mut ui, now) = scene();
        assert!(press(&mut ui, &state(), ctrl('g'), now).is_empty());
        assert!(ui.switcher.is_none());
        assert!(ui.notices.iter().any(|n| n.text == NO_AGENTS));
    }

    #[test]
    fn the_switcher_opens_on_the_child_that_is_already_in_view() {
        let mut tree = with_child(vec![]);
        tree.show(&child_id());
        let (mut ui, now) = scene();
        press_tree(&mut ui, &tree, ctrl('g'), now);
        assert_eq!(ui.switcher.map(|s| s.selected), Some(1));
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

    // ---- the plugin-state panel -----------------------------------------

    #[test]
    fn ctrl_t_toggles_the_plugin_state_panel_and_esc_closes_it() {
        let (mut ui, now) = scene();
        assert!(press(&mut ui, &state(), ctrl('t'), now).is_empty());
        assert!(ui.panel);
        press(&mut ui, &state(), ctrl('t'), now);
        assert!(!ui.panel, "the same chord closes it");

        press(&mut ui, &state(), ctrl('t'), now);
        ui.help = true;
        assert!(press(&mut ui, &state(), key(KeyCode::Esc), now).is_empty());
        assert!(!ui.panel, "esc takes the innermost panel first");
        assert!(ui.help);
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
    fn the_page_keys_scroll_and_come_back_to_the_bottom() {
        let (mut ui, now) = scene();
        press(&mut ui, &state(), key(KeyCode::PageUp), now);
        assert_eq!(ui.scroll.0, PAGE);
        press(&mut ui, &state(), key(KeyCode::PageDown), now);
        assert_eq!(ui.scroll, Scroll::default());
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
        assert_eq!(ui.scroll, Scroll::default());
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
}
