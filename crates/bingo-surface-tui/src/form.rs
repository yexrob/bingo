//! The form card: the questions of one `InteractionKind::Form`, asked
//! together (M53). A tab row of headers, the question the tabs land on, its
//! options, and — when an option carries one — the preview of the option under
//! the cursor.
//!
//! The card owns no focus of its own: the cursor of the question on screen is
//! the dialog's `focus`, so one `❯` is on the screen and the card that walks
//! its answers on a short screen still keeps the row the keyboard is on
//! (design §7). A tab that is not on screen keeps its cursor here until it is
//! walked back to.
//!
//! Nothing is sent until the whole form is: `⏎` fixes one question and steps
//! to the next that is not fixed, and the last one sends every answer in the
//! order they were asked.

use bingo_sdk::{Answer, Question, QuestionOption};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::composer::Composer;
use crate::theme;

/// From this many columns of card the preview stands beside the options; below
/// it, above them. A 120-column terminal is the wide one (design §3).
const PANE_FROM: usize = 100;

/// The narrowest the options' column is squeezed to, so a label and its
/// description are not made to wrap for the sake of the pane.
const COLUMN_LEAST: usize = 24;

/// One question's own state. The cursor of the question on screen lives in the
/// dialog; this keeps the one a tab was left on.
#[derive(Clone, Debug, Default)]
struct Slot {
    /// Where the cursor was when this tab was walked away from.
    cursor: usize,
    /// The ids ticked, in a multiple-choice question.
    chosen: Vec<String>,
    /// The words row, once it has been opened.
    words: Option<Composer>,
    /// Whether `⏎` has fixed this question.
    fixed: bool,
}

/// The surface's own state for a form.
#[derive(Clone, Debug, Default)]
pub struct Form {
    /// Which question is on screen.
    pub tab: usize,
    /// One per question, grown as the tabs are walked.
    slots: Vec<Slot>,
}

impl Form {
    fn slot(&self, tab: usize) -> Option<&Slot> {
        self.slots.get(tab)
    }

    fn slot_mut(&mut self, tab: usize) -> &mut Slot {
        if self.slots.len() <= tab {
            self.slots.resize_with(tab + 1, Slot::default);
        }
        &mut self.slots[tab]
    }

    fn fixed(&self, tab: usize) -> bool {
        self.slot(tab).is_some_and(|slot| slot.fixed)
    }

    /// The first question after `from` that is not fixed yet, wrapping once.
    fn unfixed(&self, from: usize, count: usize) -> Option<usize> {
        (1..=count)
            .map(|step| (from + step) % count)
            .find(|tab| !self.fixed(*tab))
    }
}

/// The rows one question offers: its options, then the row where a person
/// answers in words of their own.
fn row_count(question: &Question) -> usize {
    question.options.len() + usize::from(question.free_text)
}

/// Whether the cursor is on the words row rather than on an option.
fn on_words(question: &Question, cursor: usize) -> bool {
    question.free_text && cursor >= question.options.len()
}

/// A key on the form. `Some` is the answer to send: every question's, in the
/// order they were asked. Leaving the card is the dialog's own `esc`.
pub fn on_key(
    form: &mut Form,
    focus: &mut usize,
    questions: &[Question],
    key: KeyEvent,
) -> Option<Answer> {
    let question = questions.get(form.tab)?;
    if typing(form, question, *focus) {
        return typed_key(form, focus, questions, key);
    }
    match key.code {
        // Not `shift+tab`: that cycles the mode wherever a person is (§4).
        KeyCode::Left => walk(form, focus, questions.len(), -1),
        KeyCode::Right | KeyCode::Tab => walk(form, focus, questions.len(), 1),
        KeyCode::Up => *focus = focus.saturating_sub(1),
        KeyCode::Down => *focus = (*focus + 1).min(row_count(question).saturating_sub(1)),
        KeyCode::Char(' ') => tick(form, question, *focus),
        KeyCode::Enter => return fix(form, focus, questions),
        KeyCode::Char(c @ '1'..='9') if bare(key) => {
            let index = (c as usize) - ('1' as usize);
            if index >= row_count(question) {
                return None;
            }
            *focus = index;
            return fix(form, focus, questions);
        }
        _ => {}
    }
    None
}

/// Whether the words row is open and taking the keys.
fn typing(form: &Form, question: &Question, focus: usize) -> bool {
    on_words(question, focus) && form.slot(form.tab).is_some_and(|slot| slot.words.is_some())
}

/// The words row's own keys. `⏎` fixes the question with what was typed.
fn typed_key(
    form: &mut Form,
    focus: &mut usize,
    questions: &[Question],
    key: KeyEvent,
) -> Option<Answer> {
    let words = form.slot_mut(form.tab).words.as_mut()?;
    match key.code {
        KeyCode::Enter => return fix(form, focus, questions),
        KeyCode::Backspace => words.backspace(),
        KeyCode::Left => words.left(),
        KeyCode::Right => words.right(),
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            words.insert(&c.to_string())
        }
        _ => {}
    }
    None
}

/// Step one tab along, keeping where each one's cursor was.
fn walk(form: &mut Form, focus: &mut usize, count: usize, by: isize) {
    let Some(tab) = form.tab.checked_add_signed(by).filter(|tab| *tab < count) else {
        return;
    };
    form.slot_mut(form.tab).cursor = *focus;
    form.tab = tab;
    *focus = form.slot(tab).map_or(0, |slot| slot.cursor);
}

/// Tick or untick the option under the cursor. Only a set has boxes to tick.
fn tick(form: &mut Form, question: &Question, focus: usize) {
    if !question.multi {
        return;
    }
    let Some(option) = question.options.get(focus) else {
        return;
    };
    let chosen = &mut form.slot_mut(form.tab).chosen;
    match chosen.iter().position(|id| id == &option.id) {
        Some(at) => {
            chosen.remove(at);
        }
        None => chosen.push(option.id.clone()),
    }
}

/// `⏎`: fix this question and step to the next one still open; when they are
/// all fixed, the form is the answer.
fn fix(form: &mut Form, focus: &mut usize, questions: &[Question]) -> Option<Answer> {
    let question = questions.get(form.tab)?;
    // The words row asks before it answers: the first `⏎` opens it.
    if on_words(question, *focus) && form.slot(form.tab).is_none_or(|s| s.words.is_none()) {
        form.slot_mut(form.tab).words = Some(Composer::default());
        return None;
    }
    form.slot_mut(form.tab).cursor = *focus;
    form.slot_mut(form.tab).fixed = true;
    match form.unfixed(form.tab, questions.len()) {
        Some(tab) => {
            form.tab = tab;
            *focus = form.slot(tab).map_or(0, |slot| slot.cursor);
            None
        }
        None => Some(Answer::Form {
            answers: questions
                .iter()
                .enumerate()
                .map(|(tab, question)| answer(form, tab, question))
                .collect(),
        }),
    }
}

/// One question's answer, read off what the person left on it. A question
/// nobody settled — or settled on nothing — is a cancel in its place.
fn answer(form: &Form, tab: usize, question: &Question) -> Answer {
    let Some(slot) = form.slot(tab).filter(|slot| slot.fixed) else {
        return Answer::Cancel;
    };
    if on_words(question, slot.cursor) {
        let text = slot.words.as_ref().map_or("", |w| w.text()).trim();
        return match text.is_empty() {
            true => Answer::Cancel,
            false => Answer::Text { text: text.into() },
        };
    }
    let ids = match question.multi {
        true => slot.chosen.clone(),
        false => question
            .options
            .get(slot.cursor)
            .map(|option| vec![option.id.clone()])
            .unwrap_or_default(),
    };
    match ids.is_empty() {
        true => Answer::Cancel,
        false => Answer::Choice { ids },
    }
}

/// A bare digit, not a chord.
fn bare(key: KeyEvent) -> bool {
    (key.modifiers - KeyModifiers::SHIFT).is_empty()
}

/// The card, as the lines inside it. Every row carries the option it belongs
/// to, as the dialog's do, so a click lands where the eye is.
///
/// `width` is the room inside the box. The preview stands beside the options
/// when there is width for it and above them when there is not — never below,
/// because what gives way on a short screen is whatever sits above the
/// answers (design §2).
pub fn rows(
    form: &Form,
    focus: usize,
    questions: &[Question],
    width: usize,
) -> Vec<(Line<'static>, Option<usize>)> {
    let mut out = Vec::new();
    let Some(question) = questions.get(form.tab) else {
        return out;
    };
    let preview = preview_of(question, focus);
    let pane = preview.is_some() && width >= PANE_FROM;
    if let Some(preview) = preview.filter(|_| !pane) {
        out.extend(
            stacked(&preview, width)
                .into_iter()
                .map(|line| (line, None)),
        );
    }
    out.push((Line::default(), None));
    out.push((
        Line::from(Span::styled(question.question.clone(), theme::text())),
        None,
    ));
    let beside = match pane {
        true => preview_of(question, focus).unwrap_or_default(),
        false => String::new(),
    };
    out.extend(option_rows(form, focus, question, width, &beside));
    out
}

/// The card's own title: the headers, the one on screen bright and the ones
/// already settled ticked. Spans rather than a line, because what asked is
/// named after them and that is the dialog's to add.
pub fn tabs(form: &Form, questions: &[Question]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (tab, question) in questions.iter().enumerate() {
        if tab > 0 {
            spans.push(Span::styled(" · ", theme::dim()));
        }
        // Settled, in the mark a ticked box already wears: no seventh glyph
        // for a fact the bright header beside it also carries.
        let mark = match form.fixed(tab) {
            true => format!("{} ", theme::todo(true)),
            false => String::new(),
        };
        let name = question
            .header
            .clone()
            .unwrap_or_else(|| format!("Question {}", tab + 1));
        let style = match tab == form.tab {
            true => theme::bold(),
            false => theme::dim(),
        };
        spans.push(Span::styled(format!("{mark}{name}"), style));
    }
    spans
}

/// The preview of the option the cursor is on, when it carries one.
fn preview_of(question: &Question, focus: usize) -> Option<String> {
    question.options.get(focus)?.preview.clone()
}

/// The preview above the options, cut to the rows a card can spare.
fn stacked(preview: &str, width: usize) -> Vec<Line<'static>> {
    let rows: Vec<&str> = preview.lines().collect();
    let shown = rows.len().min(STACKED_ROWS);
    let mut out: Vec<Line<'static>> = rows[..shown]
        .iter()
        .map(|row| Line::from(Span::styled(format!("  {}", cut(row, width)), theme::dim())))
        .collect();
    if rows.len() > shown {
        out.push(Line::from(Span::styled(
            format!("  {} +{} lines", theme::ellipsis(), rows.len() - shown),
            theme::dim(),
        )));
    }
    out
}

/// How many rows of a preview stand above the options before the rest is
/// folded away: enough for a mockup, not enough to push the answers off.
const STACKED_ROWS: usize = 8;

/// The options, each with its description and — beside them, in a pane — the
/// preview of the one under the cursor.
fn option_rows(
    form: &Form,
    focus: usize,
    question: &Question,
    width: usize,
    beside: &str,
) -> Vec<(Line<'static>, Option<usize>)> {
    let mut left: Vec<(Line<'static>, Option<usize>)> = Vec::new();
    for (index, option) in question.options.iter().enumerate() {
        left.extend(one_option(form, focus, question, index, option));
    }
    if question.free_text {
        left.extend(words_rows(form, focus, question.options.len()));
    }
    match beside.is_empty() {
        true => left,
        false => paned(left, beside, width),
    }
}

/// One option: the row a key picks, its description, and its tick in a set.
fn one_option(
    form: &Form,
    focus: usize,
    question: &Question,
    index: usize,
    option: &QuestionOption,
) -> Vec<(Line<'static>, Option<usize>)> {
    let focused = index == focus;
    let style = match focused {
        true => theme::text(),
        false => theme::dim(),
    };
    let ticked = question.multi.then(|| {
        let chosen = form
            .slot(form.tab)
            .is_some_and(|slot| slot.chosen.iter().any(|id| id == &option.id));
        format!("{} ", theme::todo(chosen))
    });
    let mut out = vec![(
        Line::from(vec![
            theme::cursor_span(focused),
            Span::styled(
                format!(
                    "{}. {}{}",
                    index + 1,
                    ticked.unwrap_or_default(),
                    option.label
                ),
                style,
            ),
        ]),
        Some(index),
    )];
    if let Some(description) = &option.description {
        out.push((
            Line::from(Span::styled(format!("     {description}"), theme::dim())),
            Some(index),
        ));
    }
    out
}

/// The row where a person answers in their own words, and what they have
/// typed into it so far.
fn words_rows(form: &Form, focus: usize, index: usize) -> Vec<(Line<'static>, Option<usize>)> {
    let focused = index == focus;
    let style = match focused {
        true => theme::text(),
        false => theme::dim(),
    };
    let mut out = vec![(
        Line::from(vec![
            theme::cursor_span(focused),
            Span::styled(format!("{}. Other", index + 1), style),
        ]),
        Some(index),
    )];
    if let Some(words) = form.slot(form.tab).and_then(|slot| slot.words.as_ref()) {
        out.push((
            Line::from(vec![
                Span::styled(format!("     {} ", theme::user()), theme::dim()),
                Span::styled(words.text().to_string(), theme::text()),
            ]),
            Some(index),
        ));
    }
    out
}

/// The options on the left, the preview to their right. The pane costs no row
/// of its own, so a short screen loses none of the answers to it; a preview
/// taller than the options says how much of it is kept back.
fn paned(
    left: Vec<(Line<'static>, Option<usize>)>,
    beside: &str,
    width: usize,
) -> Vec<(Line<'static>, Option<usize>)> {
    // The options keep the room they need and the mockup gets the rest: it is
    // the thing being read, and an option label is short.
    let column = column_of(&left).max(COLUMN_LEAST);
    let pane = width.saturating_sub(column + 3);
    let rows: Vec<&str> = beside.lines().collect();
    let shown = beside_rows(rows.len(), left.len());
    left.into_iter()
        .enumerate()
        .map(|(row, (line, option))| {
            let text = match rows.get(row) {
                Some(text) if row < shown => cut(text, pane),
                _ if row == shown && rows.len() > shown => {
                    format!("{} +{} lines", theme::ellipsis(), rows.len() - shown)
                }
                _ => String::new(),
            };
            (join(line, column, &text), option)
        })
        .collect()
}

/// How wide the options' own column is: the longest row of it.
fn column_of(left: &[(Line<'static>, Option<usize>)]) -> usize {
    left.iter()
        .map(|(line, _)| line.spans.iter().map(|span| span.content.width()).sum())
        .max()
        .unwrap_or(0)
}

/// How many rows of the preview stand beside the options: all of them when
/// they fit, else one fewer than the room, so the last row can say how many
/// are left.
fn beside_rows(rows: usize, room: usize) -> usize {
    match rows <= room {
        true => rows,
        false => room.saturating_sub(1),
    }
}

/// One row of the pane: the option's line padded to its column, a wall, then
/// the preview's own row in dim.
fn join(line: Line<'static>, column: usize, text: &str) -> Line<'static> {
    let filled: usize = line.spans.iter().map(|span| span.content.width()).sum();
    let mut spans = line.spans;
    spans.push(Span::raw(" ".repeat(column.saturating_sub(filled))));
    spans.push(Span::styled(format!(" {} ", theme::wall()), theme::dim()));
    spans.push(Span::styled(text.to_string(), theme::dim()));
    Line::from(spans)
}

/// A preview's row is a mockup and never wraps: what does not fit is cut, and
/// says so.
fn cut(row: &str, width: usize) -> String {
    if row.width() <= width {
        return row.to_string();
    }
    let ellipsis = theme::ellipsis();
    let room = width.saturating_sub(ellipsis.width());
    let mut out = String::new();
    for c in row.chars() {
        if out.width() + c.to_string().width() > room {
            break;
        }
        out.push(c);
    }
    out.push_str(ellipsis);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn option(id: &str, preview: Option<&str>) -> QuestionOption {
        QuestionOption {
            id: id.into(),
            label: format!("the {id} one"),
            description: None,
            role: None,
            preview: preview.map(str::to_owned),
        }
    }

    fn question(header: &str, multi: bool, previews: bool) -> Question {
        Question {
            question: format!("which {header}?"),
            header: Some(header.into()),
            options: vec![
                option("first", previews.then_some("a\nb")),
                option("second", previews.then_some("c")),
            ],
            free_text: true,
            multi,
        }
    }

    fn three() -> Vec<Question> {
        vec![
            question("Auth", false, false),
            question("Library", false, false),
            question("Targets", true, false),
        ]
    }

    /// Every key of one walk, so a test reads as the hands do.
    fn press(
        form: &mut Form,
        focus: &mut usize,
        questions: &[Question],
        keys: &[KeyCode],
    ) -> Option<Answer> {
        let mut answer = None;
        for code in keys {
            answer = on_key(form, focus, questions, key(*code));
        }
        answer
    }

    fn chose(id: &str) -> Answer {
        Answer::Choice {
            ids: vec![id.into()],
        }
    }

    #[test]
    fn enter_fixes_one_question_and_the_last_sends_them_all_in_order() {
        let questions = three();
        let (mut form, mut focus) = (Form::default(), 0);
        assert_eq!(
            press(&mut form, &mut focus, &questions, &[KeyCode::Enter]),
            None,
            "the first of three settles nothing on its own"
        );
        assert_eq!(form.tab, 1, "and the card steps to the next still open");
        // The second on its second option, the third with one target ticked.
        let answer = press(
            &mut form,
            &mut focus,
            &questions,
            &[
                KeyCode::Down,
                KeyCode::Enter,
                KeyCode::Char(' '),
                KeyCode::Enter,
            ],
        );
        assert_eq!(
            answer,
            Some(Answer::Form {
                answers: vec![chose("first"), chose("second"), chose("first")],
            })
        );
    }

    #[test]
    fn the_arrows_walk_the_tabs_and_each_keeps_the_cursor_it_was_left_on() {
        let questions = three();
        let (mut form, mut focus) = (Form::default(), 0);
        press(
            &mut form,
            &mut focus,
            &questions,
            &[KeyCode::Down, KeyCode::Right],
        );
        assert_eq!(
            (form.tab, focus),
            (1, 0),
            "a tab not yet visited starts at its first row"
        );
        press(&mut form, &mut focus, &questions, &[KeyCode::Left]);
        assert_eq!(
            (form.tab, focus),
            (0, 1),
            "and the one walked away from is where it was"
        );
        press(&mut form, &mut focus, &questions, &[KeyCode::Left]);
        assert_eq!(form.tab, 0, "the first tab has nothing to its left");
    }

    #[test]
    fn tab_walks_the_questions_as_the_arrows_do() {
        let questions = three();
        let (mut form, mut focus) = (Form::default(), 0);
        press(
            &mut form,
            &mut focus,
            &questions,
            &[KeyCode::Tab, KeyCode::Tab],
        );
        assert_eq!(form.tab, 2);
        press(&mut form, &mut focus, &questions, &[KeyCode::Tab]);
        assert_eq!(form.tab, 2, "and stops at the last");
    }

    #[test]
    fn space_ticks_a_set_and_a_choice_has_no_boxes_to_tick() {
        let questions = three();
        let (mut form, mut focus) = (Form::default(), 0);
        press(&mut form, &mut focus, &questions, &[KeyCode::Char(' ')]);
        assert!(form.slot(0).is_none_or(|slot| slot.chosen.is_empty()));
        form.tab = 2;
        press(
            &mut form,
            &mut focus,
            &questions,
            &[KeyCode::Char(' '), KeyCode::Down, KeyCode::Char(' ')],
        );
        assert_eq!(
            form.slot(2).map(|slot| slot.chosen.clone()),
            Some(vec!["first".to_string(), "second".to_string()])
        );
        press(&mut form, &mut focus, &questions, &[KeyCode::Char(' ')]);
        assert_eq!(
            form.slot(2).map(|slot| slot.chosen.clone()),
            Some(vec!["first".to_string()]),
            "the same key unticks it"
        );
    }

    #[test]
    fn a_digit_picks_the_row_it_names_and_fixes_it() {
        let questions = three();
        let (mut form, mut focus) = (Form::default(), 0);
        press(&mut form, &mut focus, &questions, &[KeyCode::Char('2')]);
        assert!(form.fixed(0) && form.tab == 1);
        assert_eq!(
            press(&mut form, &mut focus, &questions, &[KeyCode::Char('9')]),
            None,
            "a digit no row answers to does nothing"
        );
        assert!(!form.fixed(1));
    }

    #[test]
    fn the_words_row_answers_in_the_persons_own_words() {
        let questions = vec![question("Auth", false, false)];
        let (mut form, mut focus) = (Form::default(), 2);
        // The first `⏎` opens the row, then what is typed is the answer.
        assert_eq!(
            press(&mut form, &mut focus, &questions, &[KeyCode::Enter]),
            None
        );
        let answer = press(
            &mut form,
            &mut focus,
            &questions,
            &[
                KeyCode::Char('n'),
                KeyCode::Char('o'),
                KeyCode::Char('!'),
                KeyCode::Enter,
            ],
        );
        assert_eq!(
            answer,
            Some(Answer::Form {
                answers: vec![Answer::Text { text: "no!".into() }],
            })
        );
    }

    #[test]
    fn a_question_settled_on_nothing_is_a_cancel_in_its_place() {
        let questions = vec![
            question("Auth", false, false),
            question("Targets", true, false),
        ];
        let (mut form, mut focus) = (Form::default(), 0);
        // The first left to the words row with nothing typed; the second a set
        // with nothing ticked.
        press(
            &mut form,
            &mut focus,
            &questions,
            &[KeyCode::Down, KeyCode::Down, KeyCode::Enter, KeyCode::Enter],
        );
        let answer = press(&mut form, &mut focus, &questions, &[KeyCode::Enter]);
        assert_eq!(
            answer,
            Some(Answer::Form {
                answers: vec![Answer::Cancel, Answer::Cancel],
            })
        );
    }

    /// One row per line of the card, as a person reads them.
    fn drawn(form: &Form, focus: usize, questions: &[Question], width: usize) -> Vec<String> {
        rows(form, focus, questions, width)
            .into_iter()
            .map(|(line, _)| line.spans.iter().map(|s| s.content.to_string()).collect())
            .collect()
    }

    #[test]
    fn the_preview_stands_beside_the_option_under_the_cursor_when_there_is_width() {
        let questions = vec![question("Auth", false, true)];
        let drawn = drawn(&Form::default(), 0, &questions, 110);
        assert!(
            drawn
                .iter()
                .any(|row| row.contains("the first one") && row.contains('a')),
            "{drawn:#?}"
        );
        assert!(
            !drawn.iter().any(|row| row.trim_start().starts_with('a')),
            "and it takes no row of its own: {drawn:#?}"
        );
    }

    #[test]
    fn a_narrow_card_puts_the_preview_above_the_answers_never_below() {
        let questions = vec![question("Auth", false, true)];
        let drawn = drawn(&Form::default(), 0, &questions, 60);
        let preview = drawn.iter().position(|row| row.trim() == "a");
        let option = drawn.iter().position(|row| row.contains("the first one"));
        assert!(preview < option, "{drawn:#?}");
    }

    #[test]
    fn the_cursor_moving_moves_the_preview_with_it() {
        let questions = vec![question("Auth", false, true)];
        let drawn = drawn(&Form::default(), 1, &questions, 60);
        assert!(drawn.iter().any(|row| row.trim() == "c"), "{drawn:#?}");
        assert!(!drawn.iter().any(|row| row.trim() == "a"), "{drawn:#?}");
    }

    #[test]
    fn a_preview_taller_than_the_options_says_how_much_it_kept_back() {
        assert_eq!(beside_rows(3, 5), 3);
        assert_eq!(beside_rows(9, 5), 4, "one row goes to saying so");
        assert_eq!(beside_rows(9, 0), 0);
    }

    #[test]
    fn a_row_wider_than_its_column_is_cut_and_says_so() {
        assert_eq!(cut("abcdef", 6), "abcdef");
        assert_eq!(cut("abcdef", 4), format!("abc{}", theme::ellipsis()));
    }

    #[test]
    fn a_tab_that_is_settled_wears_the_mark_of_one() {
        let questions = three();
        let (mut form, mut focus) = (Form::default(), 0);
        press(&mut form, &mut focus, &questions, &[KeyCode::Enter]);
        let row: String = tabs(&form, &questions)
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert_eq!(
            row,
            format!("{} Auth · Library · Targets", theme::todo(true))
        );
    }
}
