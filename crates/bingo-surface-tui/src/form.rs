//! The form card: the questions of one `InteractionKind::Form`, asked
//! together (M53) and drawn since M57 the way Claude Code draws its own, at the
//! user's word — a band between two dim rules rather than a box, a tab per
//! question wearing the box it is ticked in, a `Submit` tab at the end of the
//! walk, the option's preview in a frame of its own, and the keys said on the
//! card.
//!
//! The card owns no focus of its own: the cursor of the row on screen is the
//! dialog's `focus`, so one `❯` is on the screen and the card that walks its
//! answers on a short screen still keeps the row the keyboard is on
//! (design §7). A tab that is not on screen keeps its cursor here until it is
//! walked back to.
//!
//! Nothing is sent until a person says so on the last tab: `⏎` fixes one
//! question and steps to the next that is not fixed, and `⏎` on `Submit` sends
//! every answer in the order they were asked — or, with one question still
//! open, walks to it.

use bingo_sdk::{Answer, Question, QuestionOption};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::composer::Composer;
use crate::{layers, theme};

/// From this many columns of card the preview stands beside the options; below
/// it, above them. A 120-column terminal is the wide one (design §3).
const PANE_FROM: usize = 100;

/// Cells between the options' own column and the preview's frame.
const GAP: usize = 2;

/// How many rows of a mockup its frame holds before the rest is folded away:
/// enough for a mockup, not enough to push the answers off.
const PREVIEW_ROWS: usize = 8;

/// The row that answers in a person's own words, in Claude Code's words for it.
const WORDS: &str = "Type something.";

/// The row that leaves the questions and talks to the model instead.
const CHAT: &str = "Chat about this";

/// The tab that sends: what it is called, and the two rows it offers.
const SUBMIT: &str = "Submit";
const SUBMIT_ROWS: usize = 2;

/// One question's own state. The cursor of the row on screen lives in the
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
    /// Which tab is on screen: a question, or [`submit_tab`].
    pub tab: usize,
    /// One per tab, grown as they are walked.
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

    /// The first question, in the order they were asked, that is not fixed.
    fn open(&self, count: usize) -> Option<usize> {
        (0..count).find(|tab| !self.fixed(*tab))
    }

    /// How many questions are still open, which is what `Submit` says.
    fn open_count(&self, count: usize) -> usize {
        (0..count).filter(|tab| !self.fixed(*tab)).count()
    }
}

/// The tab after the last question: the one that sends.
fn submit_tab(questions: &[Question]) -> usize {
    questions.len()
}

/// Which row of a question answers in the person's own words, where it takes
/// one at all.
fn words_at(question: &Question) -> Option<usize> {
    question.free_text.then_some(question.options.len())
}

/// Which row of a question leaves it for the composer: the last, under the
/// band's own rule.
fn chat_at(question: &Question) -> usize {
    question.options.len() + usize::from(question.free_text)
}

/// The rows the tab on screen offers a cursor.
fn rows_on(form: &Form, questions: &[Question]) -> usize {
    match questions.get(form.tab) {
        Some(question) => chat_at(question) + 1,
        None => SUBMIT_ROWS,
    }
}

/// Whether the cursor is on the words row rather than on an option.
fn on_words(question: &Question, cursor: usize) -> bool {
    words_at(question) == Some(cursor)
}

/// Whether any option of this question carries a mockup.
fn has_preview(question: &Question) -> bool {
    question.options.iter().any(|o| o.preview.is_some())
}

/// A key on the form. `Some` is the answer to send: every question's, in the
/// order they were asked, or the cancel a person chose. Leaving the card is the
/// dialog's own `esc`.
pub fn on_key(
    form: &mut Form,
    focus: &mut usize,
    questions: &[Question],
    key: KeyEvent,
) -> Option<Answer> {
    if typing(form, questions, *focus) {
        return typed_key(form, focus, questions, key);
    }
    let tabs = submit_tab(questions) + 1;
    match key.code {
        // Not `shift+tab`: that cycles the mode wherever a person is (§4).
        KeyCode::Left => walk(form, focus, tabs, -1),
        KeyCode::Right | KeyCode::Tab => walk(form, focus, tabs, 1),
        KeyCode::Up => *focus = focus.saturating_sub(1),
        KeyCode::Down => *focus = (*focus + 1).min(rows_on(form, questions) - 1),
        KeyCode::Char(' ') => tick(form, questions, *focus),
        KeyCode::Enter => return chosen(form, focus, questions),
        KeyCode::Char(c @ '1'..='9') if bare(key) => {
            let index = (c as usize) - ('1' as usize);
            if index >= rows_on(form, questions) {
                return None;
            }
            *focus = index;
            return chosen(form, focus, questions);
        }
        _ => {}
    }
    None
}

/// Whether the words row is open and taking the keys.
fn typing(form: &Form, questions: &[Question], focus: usize) -> bool {
    questions
        .get(form.tab)
        .is_some_and(|question| on_words(question, focus))
        && form.slot(form.tab).is_some_and(|slot| slot.words.is_some())
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
    go(form, focus, tab);
}

/// Land on a tab, on the row it was left on.
fn go(form: &mut Form, focus: &mut usize, tab: usize) {
    form.tab = tab;
    *focus = form.slot(tab).map_or(0, |slot| slot.cursor);
}

/// Tick or untick the option under the cursor. Only a set has boxes to tick.
fn tick(form: &mut Form, questions: &[Question], focus: usize) {
    let Some(question) = questions.get(form.tab).filter(|q| q.multi) else {
        return;
    };
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

/// `⏎`, or the digit that stands for a row: what the row under the cursor does.
fn chosen(form: &mut Form, focus: &mut usize, questions: &[Question]) -> Option<Answer> {
    match questions.get(form.tab) {
        Some(question) => on_question(form, focus, questions, question),
        None => on_submit(form, focus, questions),
    }
}

/// A row of one question: the way out, the words row that opens before it
/// answers, or an answer fixed.
fn on_question(
    form: &mut Form,
    focus: &mut usize,
    questions: &[Question],
    question: &Question,
) -> Option<Answer> {
    if *focus == chat_at(question) {
        return Some(Answer::Cancel);
    }
    if on_words(question, *focus) && form.slot(form.tab).is_none_or(|s| s.words.is_none()) {
        form.slot_mut(form.tab).words = Some(Composer::default());
        return None;
    }
    fix(form, focus, questions)
}

/// A row of the `Submit` tab: send every answer, or — with a question still
/// open — walk to the first one, which is what Claude Code does.
fn on_submit(form: &mut Form, focus: &mut usize, questions: &[Question]) -> Option<Answer> {
    if *focus != 0 {
        return Some(Answer::Cancel);
    }
    if let Some(tab) = form.open(questions.len()) {
        form.slot_mut(form.tab).cursor = *focus;
        go(form, focus, tab);
        return None;
    }
    Some(Answer::Form {
        answers: questions
            .iter()
            .enumerate()
            .map(|(tab, question)| answer(form, tab, question))
            .collect(),
    })
}

/// Fix this question and step to the next one still open; when they are all
/// fixed, to the tab that sends.
fn fix(form: &mut Form, focus: &mut usize, questions: &[Question]) -> Option<Answer> {
    form.slot_mut(form.tab).cursor = *focus;
    form.slot_mut(form.tab).fixed = true;
    let next = form
        .unfixed(form.tab, questions.len())
        .unwrap_or_else(|| submit_tab(questions));
    go(form, focus, next);
    None
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

/// What the card is asked under: what the whole set is about where the asker
/// said, who asked where it was not this session, and the room it has.
#[derive(Clone, Copy, Debug)]
pub struct Head<'a> {
    pub title: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub width: usize,
    /// The rows the card's own lines have. What does not fit is given away in
    /// the order [`Card::within`] gives it.
    pub room: usize,
}

/// One line of the card and the row of the question it belongs to, which is
/// what a click lands on.
type Row = (Line<'static>, Option<usize>);

/// The card in the parts that give way in order. An answer is what the card
/// was opened for, so the keys go first and the mockup above the options next;
/// what is left is cut by [`crate::layers::card`], which keeps the first row
/// and the newest (design §2).
struct Card {
    /// The title where the asker gave one, and the tab row.
    head: Vec<Row>,
    /// The one line that asks, or what `Submit` will do.
    asks: Vec<Row>,
    /// The mockup, where it stands above the options rather than beside them.
    above: Vec<Row>,
    /// The options, the words row, the row that sends.
    answers: Vec<Row>,
    /// The rule that closes the band, and the way out under it.
    tail: Vec<Row>,
    keys: Row,
}

impl Card {
    /// The card in the room it has: the keys are the first row it gives up and
    /// the mockup the next, because a frame half drawn is worse than none.
    fn within(self, room: usize) -> Vec<Row> {
        let whole = self.head.len()
            + self.asks.len()
            + self.above.len()
            + self.answers.len()
            + self.tail.len();
        let keys = whole < room;
        let mut out = self.head;
        out.extend(self.asks);
        if keys || whole <= room {
            out.extend(self.above);
        }
        out.extend(self.answers);
        out.extend(self.tail);
        if keys {
            out.push(self.keys);
        }
        out
    }
}

/// The card, as the lines inside it. Every row carries the option it belongs
/// to, as the dialog's do, so a click lands where the eye is.
///
/// The first row is what [`crate::layers::card`] keeps whatever else gives
/// way: the title where the asker gave one, else the tabs. Then the question
/// and its options, the rule that closes the band, the way out and the keys.
/// The preview stands beside the options where there is width for it and above
/// them where there is not — never below, because what gives way on a short
/// screen is whatever sits above the answers (design §2).
pub fn rows(form: &Form, focus: usize, questions: &[Question], at: Head<'_>) -> Vec<Row> {
    let (asks, above, answers) = match questions.get(form.tab) {
        Some(question) => asked(form, focus, question, at.width),
        None => sending(form, focus, questions),
    };
    Card {
        head: heading(form, questions, at),
        asks,
        above,
        answers,
        tail: vec![
            (layers::rule(at.width), None),
            numbered(chat_row(form, questions), focus, CHAT),
        ],
        keys: (keys(form, questions), None),
    }
    .within(at.room)
}

/// What the card opens with: the title where there is one, and the tab row.
fn heading(form: &Form, questions: &[Question], at: Head<'_>) -> Vec<Row> {
    let tabs = tabs(form, questions);
    match at.title {
        Some(title) => vec![
            (Line::from(head(title, at.agent)), None),
            (Line::from(tabs), None),
        ],
        None => vec![(Line::from(with_agent(tabs, at.agent)), None)],
    }
}

/// One card's head: what it is, and who asked where that was not the session
/// on screen (ADR-0010 §3). Every card's, so a form's title and a permission's
/// tool name are named the same way.
pub fn head(title: &str, agent: Option<&str>) -> Vec<Span<'static>> {
    with_agent(vec![Span::styled(title.to_string(), theme::bold())], agent)
}

fn with_agent(mut spans: Vec<Span<'static>>, agent: Option<&str>) -> Vec<Span<'static>> {
    if let Some(agent) = agent {
        spans.push(Span::styled(format!(" · {agent}"), theme::presence()));
    }
    spans
}

/// The tab row: an arrow at each end, a box per question, and the tab that
/// sends. The one on screen is in `text` and the rest are dim (§7); an arrow
/// with nowhere to go is dim too.
fn tabs(form: &Form, questions: &[Question]) -> Vec<Span<'static>> {
    let last = submit_tab(questions);
    let mut spans = vec![arrow("←", form.tab > 0)];
    for (tab, question) in questions.iter().enumerate() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("{} {}", theme::todo(form.fixed(tab)), name(question, tab)),
            weight(tab == form.tab),
        ));
    }
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("{} {SUBMIT}", theme::tick()),
        weight(form.tab == last),
    ));
    spans.push(Span::raw("  "));
    spans.push(arrow("→", form.tab < last));
    spans
}

/// One end of the tab row: bright while there is a tab that way, dim where the
/// walk stops.
fn arrow(mark: &str, live: bool) -> Span<'static> {
    Span::styled(mark.to_string(), weight(live))
}

/// The weight of the tab a person is on, and of the ones they are not.
fn weight(here: bool) -> ratatui::style::Style {
    match here {
        true => theme::text(),
        false => theme::dim(),
    }
}

fn name(question: &Question, tab: usize) -> String {
    question
        .header
        .clone()
        .unwrap_or_else(|| format!("Question {}", tab + 1))
}

/// The question on screen, in the three parts of [`Card`] it fills: what it
/// asks, the mockup where that stands above the options rather than beside
/// them, and the rows a person answers on.
fn asked(
    form: &Form,
    focus: usize,
    question: &Question,
    width: usize,
) -> (Vec<Row>, Vec<Row>, Vec<Row>) {
    let asks = vec![(
        Line::from(Span::styled(question.question.clone(), theme::text())),
        None,
    )];
    let options = option_rows(form, focus, question, has_preview(question));
    match preview_of(question, focus) {
        Some(preview) if width >= PANE_FROM => (asks, Vec::new(), beside(options, &preview, width)),
        Some(preview) => (
            asks,
            framed(&preview, width)
                .into_iter()
                .map(|l| (l, None))
                .collect(),
            options,
        ),
        None => (asks, Vec::new(), options),
    }
}

/// The `Submit` tab: what `⏎` there will do, and the row it does it from.
fn sending(form: &Form, focus: usize, questions: &[Question]) -> (Vec<Row>, Vec<Row>, Vec<Row>) {
    let says = match form.open_count(questions.len()) {
        0 => "Send the answers.".to_string(),
        1 => "1 question is still open.".to_string(),
        open => format!("{open} questions are still open."),
    };
    (
        vec![(Line::from(Span::styled(says, theme::text())), None)],
        Vec::new(),
        vec![numbered(0, focus, SUBMIT)],
    )
}

/// Which row of the tab on screen leaves it for the composer.
fn chat_row(form: &Form, questions: &[Question]) -> usize {
    match questions.get(form.tab) {
        Some(question) => chat_at(question),
        None => SUBMIT_ROWS - 1,
    }
}

/// The keys the card answers to, said on the card the way a permission's
/// answers say theirs (§7).
fn keys(form: &Form, questions: &[Question]) -> Line<'static> {
    let mut said = vec!["Enter to select"];
    if questions.get(form.tab).is_some_and(|q| q.multi) {
        said.push("Space to toggle");
    }
    said.extend([
        "↑/↓ to navigate",
        "Tab to switch questions",
        "Esc to cancel",
    ]);
    Line::from(Span::styled(said.join(" · "), theme::dim()))
}

/// The preview of the option the cursor is on, when it carries one.
fn preview_of(question: &Question, focus: usize) -> Option<String> {
    question.options.get(focus)?.preview.clone()
}

/// The options, each numbered, with its description under it — unless the
/// question carries previews, and then the labels stand alone so the mockup is
/// what is read.
fn option_rows(form: &Form, focus: usize, question: &Question, compact: bool) -> Vec<Row> {
    let mut out = Vec::new();
    for (index, option) in question.options.iter().enumerate() {
        out.push(numbered(index, focus, &labelled(form, question, option)));
        if let Some(description) = option.description.as_ref().filter(|_| !compact) {
            out.push((described(description), Some(index)));
        }
    }
    if let Some(index) = words_at(question) {
        out.extend(words_rows(form, focus, index));
    }
    out
}

/// One row a key or a click picks: the cursor, the number it answers to, and
/// its words. Every numbered row of the card is this one.
fn numbered(index: usize, focus: usize, label: &str) -> Row {
    let focused = index == focus;
    (
        Line::from(vec![
            theme::cursor_span(focused),
            Span::styled(format!("{}. {label}", index + 1), weight(focused)),
        ]),
        Some(index),
    )
}

fn described(description: &str) -> Line<'static> {
    Line::from(Span::styled(format!("     {description}"), theme::dim()))
}

/// One option's words: the box it wears in a set, then its label.
fn labelled(form: &Form, question: &Question, option: &QuestionOption) -> String {
    match question.multi {
        false => option.label.clone(),
        true => format!("{} {}", ticked(form, option), option.label),
    }
}

/// A member of a set, ticked or not: brackets in either look, and the tick the
/// glyph table spells — `[✔]`, `[x]` where nothing but ASCII may be drawn.
fn ticked(form: &Form, option: &QuestionOption) -> String {
    let chosen = form
        .slot(form.tab)
        .is_some_and(|slot| slot.chosen.iter().any(|id| id == &option.id));
    let mark = match chosen {
        true => theme::tick(),
        false => " ",
    };
    format!("[{mark}]")
}

/// The row where a person answers in their own words, and what they have
/// typed into it so far.
fn words_rows(form: &Form, focus: usize, index: usize) -> Vec<Row> {
    let mut out = vec![numbered(index, focus, WORDS)];
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

/// The options on the left and the framed preview to their right, from the
/// column after the longest of them. The frame costs no row of its own while
/// it is no taller than the options; where it is taller, its own rows close it.
fn beside(options: Vec<Row>, preview: &str, width: usize) -> Vec<Row> {
    let column = column_of(&options) + GAP;
    let frame = framed(preview, width.saturating_sub(column));
    let rows = options.len().max(frame.len());
    (0..rows)
        .map(|row| {
            let (line, option) = options.get(row).cloned().unwrap_or_default();
            (join(line, column, frame.get(row)), option)
        })
        .collect()
}

/// How wide the options' own column is: the longest row of it.
fn column_of(options: &[Row]) -> usize {
    options
        .iter()
        .map(|(line, _)| filled(line))
        .max()
        .unwrap_or(0)
}

fn filled(line: &Line<'static>) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

/// One row of the pair: the option's line held out to the preview's column,
/// then the frame's own row.
fn join(line: Line<'static>, column: usize, right: Option<&Line<'static>>) -> Line<'static> {
    let Some(right) = right else {
        return line;
    };
    let pad = column.saturating_sub(filled(&line));
    let mut spans = line.spans;
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(right.spans.iter().cloned());
    Line::from(spans)
}

/// A mockup inside a dim single-line frame that hugs its widest row with one
/// cell of padding, so a mockup drawn out of box characters is never read as
/// the card's own edge.
fn framed(preview: &str, room: usize) -> Vec<Line<'static>> {
    let edge = theme::border();
    let Some(inner) = room.checked_sub(4).filter(|room| *room > 0) else {
        return Vec::new();
    };
    let rows = kept(preview, inner);
    let wide = rows.iter().map(|row| row.width()).max().unwrap_or(0);
    let mut out = vec![framing(
        edge.top_left,
        edge.horizontal_top,
        edge.top_right,
        wide,
    )];
    out.extend(rows.iter().map(|row| held(row, wide, edge)));
    out.push(framing(
        edge.bottom_left,
        edge.horizontal_bottom,
        edge.bottom_right,
        wide,
    ));
    out
}

/// One edge of the frame: a corner, the stroke across, a corner.
fn framing(left: &str, stroke: &str, right: &str, wide: usize) -> Line<'static> {
    Line::from(Span::styled(
        format!("{left}{}{right}", stroke.repeat(wide + 2)),
        theme::dim(),
    ))
}

/// One row of the mockup, verbatim and left-aligned inside the frame.
fn held(row: &str, wide: usize, edge: ratatui::symbols::border::Set<'static>) -> Line<'static> {
    let pad = " ".repeat(wide.saturating_sub(row.width()));
    Line::from(Span::styled(
        format!("{} {row}{pad} {}", edge.vertical_left, edge.vertical_right),
        theme::dim(),
    ))
}

/// The rows of a mockup the frame holds, each cut to the room it has, and a
/// last row saying how many were kept back.
fn kept(preview: &str, room: usize) -> Vec<String> {
    let rows: Vec<&str> = preview.lines().collect();
    let shown = rows.len().min(PREVIEW_ROWS);
    let mut out: Vec<String> = rows[..shown].iter().map(|row| cut(row, room)).collect();
    if rows.len() > shown {
        let left = rows.len() - shown;
        out.push(cut(&format!("{} +{left} lines", theme::ellipsis()), room));
    }
    out
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
    fn enter_fixes_one_question_and_submit_sends_them_all_in_order() {
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
        assert_eq!(answer, None, "the last question fixed is not the form sent");
        assert_eq!(form.tab, submit_tab(&questions), "the walk lands on Submit");
        assert_eq!(
            press(&mut form, &mut focus, &questions, &[KeyCode::Enter]),
            Some(Answer::Form {
                answers: vec![chose("first"), chose("second"), chose("first")],
            })
        );
    }

    #[test]
    fn submit_with_a_question_still_open_walks_to_it_instead_of_sending() {
        let questions = three();
        let (mut form, mut focus) = (Form::default(), 0);
        // Straight to Submit without answering anything.
        press(
            &mut form,
            &mut focus,
            &questions,
            &[KeyCode::Tab, KeyCode::Tab, KeyCode::Tab],
        );
        assert_eq!(form.tab, submit_tab(&questions));
        assert_eq!(
            press(&mut form, &mut focus, &questions, &[KeyCode::Enter]),
            None,
            "nothing is sent while a question is open"
        );
        assert_eq!(form.tab, 0, "the card goes to the first one still open");
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
    fn tab_walks_the_questions_and_stops_at_submit() {
        let questions = three();
        let (mut form, mut focus) = (Form::default(), 0);
        for _ in 0..5 {
            press(&mut form, &mut focus, &questions, &[KeyCode::Tab]);
        }
        assert_eq!(form.tab, submit_tab(&questions), "and stops at the last");
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
        press(
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
            press(&mut form, &mut focus, &questions, &[KeyCode::Enter]),
            Some(Answer::Form {
                answers: vec![Answer::Text { text: "no!".into() }],
            })
        );
    }

    #[test]
    fn the_chat_row_cancels_the_whole_form() {
        let questions = three();
        let (mut form, mut focus) = (Form::default(), 0);
        // Two options, the words row, then the way out.
        assert_eq!(
            press(&mut form, &mut focus, &questions, &[KeyCode::Char('4')]),
            Some(Answer::Cancel)
        );
        let (mut form, mut focus) = (Form::default(), 0);
        press(
            &mut form,
            &mut focus,
            &questions,
            &[KeyCode::Tab, KeyCode::Tab, KeyCode::Tab, KeyCode::Down],
        );
        assert_eq!(
            press(&mut form, &mut focus, &questions, &[KeyCode::Enter]),
            Some(Answer::Cancel),
            "and the Submit tab has the same way out"
        );
    }

    #[test]
    fn a_question_settled_on_nothing_is_a_cancel_in_its_place() {
        let questions = vec![
            question("Auth", false, true),
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
        press(&mut form, &mut focus, &questions, &[KeyCode::Enter]);
        assert_eq!(
            press(&mut form, &mut focus, &questions, &[KeyCode::Enter]),
            Some(Answer::Form {
                answers: vec![Answer::Cancel, Answer::Cancel],
            })
        );
    }

    /// One row per line of the card, as a person reads them.
    fn drawn(form: &Form, focus: usize, questions: &[Question], width: usize) -> Vec<String> {
        rows(
            form,
            focus,
            questions,
            Head {
                title: None,
                agent: None,
                width,
                room: usize::MAX,
            },
        )
        .into_iter()
        .map(|(line, _)| line.spans.iter().map(|s| s.content.to_string()).collect())
        .collect()
    }

    #[test]
    fn the_tab_row_wears_a_box_per_question_an_arrow_each_end_and_submit() {
        let questions = three();
        let (mut form, mut focus) = (Form::default(), 0);
        press(&mut form, &mut focus, &questions, &[KeyCode::Enter]);
        let row: String = tabs(&form, &questions)
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert_eq!(
            row,
            format!(
                "←  {done} Auth  {open} Library  {open} Targets  {tick} Submit  →",
                done = theme::todo(true),
                open = theme::todo(false),
                tick = theme::tick(),
            )
        );
    }

    #[test]
    fn the_preview_stands_in_a_frame_beside_the_option_under_the_cursor() {
        let questions = vec![question("Auth", false, true)];
        let drawn = drawn(&Form::default(), 0, &questions, 110);
        let edge = theme::border();
        assert!(
            drawn
                .iter()
                .any(|row| row.contains("the first one") && row.contains(edge.top_left)),
            "{drawn:#?}"
        );
        assert!(
            drawn.iter().any(|row| row.contains(edge.bottom_right)),
            "the frame closes: {drawn:#?}"
        );
        assert!(
            !drawn.iter().any(|row| row.contains("     the first")),
            "a previewed question shows no descriptions: {drawn:#?}"
        );
    }

    #[test]
    fn a_narrow_card_puts_the_framed_preview_above_the_answers_never_below() {
        let questions = vec![question("Auth", false, true)];
        let drawn = drawn(&Form::default(), 0, &questions, 60);
        let preview = drawn.iter().position(|row| row.contains(" a "));
        let option = drawn.iter().position(|row| row.contains("the first one"));
        assert!(preview < option, "{drawn:#?}");
    }

    #[test]
    fn the_cursor_moving_moves_the_preview_with_it() {
        let questions = vec![question("Auth", false, true)];
        let drawn = drawn(&Form::default(), 1, &questions, 60);
        assert!(drawn.iter().any(|row| row.contains(" c ")), "{drawn:#?}");
        assert!(!drawn.iter().any(|row| row.contains(" a ")), "{drawn:#?}");
    }

    #[test]
    fn the_frame_hugs_the_mockup_and_cuts_a_row_too_wide_for_its_room() {
        let hugged = framed("ab\nc", 20);
        let drawn: Vec<String> = hugged
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        let edge = theme::border();
        assert_eq!(
            drawn[1],
            format!("{} ab {}", edge.vertical_left, edge.vertical_right)
        );
        assert_eq!(
            drawn[2],
            format!("{} c  {}", edge.vertical_left, edge.vertical_right)
        );
        assert_eq!(
            drawn[0].width(),
            6,
            "two corners, a cell of padding each side, and the mockup's own width"
        );
        assert!(
            framed(&"x".repeat(40), 12)
                .iter()
                .all(|line| { line.spans.iter().map(|s| s.content.width()).sum::<usize>() <= 12 })
        );
        assert!(framed("a", 3).is_empty(), "no room, no frame");
    }

    #[test]
    fn a_mockup_taller_than_the_frame_says_how_much_it_kept_back() {
        let tall: String = (0..12).map(|i| format!("row {i}\n")).collect();
        let rows = kept(&tall, 40);
        assert_eq!(rows.len(), PREVIEW_ROWS + 1);
        assert_eq!(
            rows[PREVIEW_ROWS],
            format!("{} +4 lines", theme::ellipsis())
        );
    }

    #[test]
    fn a_row_wider_than_its_column_is_cut_and_says_so() {
        assert_eq!(cut("abcdef", 6), "abcdef");
        assert_eq!(cut("abcdef", 4), format!("abc{}", theme::ellipsis()));
    }

    #[test]
    fn the_key_line_is_the_cards_last_row_and_a_set_adds_its_own_key() {
        let questions = three();
        let one = drawn(&Form::default(), 0, &questions, 80);
        let last = one.last().cloned().unwrap_or_default();
        assert_eq!(
            last,
            "Enter to select · ↑/↓ to navigate · Tab to switch questions · Esc to cancel"
        );
        let form = Form {
            tab: 2,
            ..Form::default()
        };
        let set = drawn(&form, 0, &questions, 80);
        assert!(
            set.last()
                .is_some_and(|row| row.contains("Space to toggle")),
            "{set:#?}"
        );
    }

    #[test]
    fn a_set_wears_brackets_and_the_last_row_of_a_question_is_the_way_out() {
        let questions = three();
        let mut form = Form {
            tab: 2,
            ..Form::default()
        };
        form.slot_mut(2).chosen = vec!["first".to_string()];
        let drawn = drawn(&form, 0, &questions, 80);
        assert!(
            drawn
                .iter()
                .any(|row| row.contains(&format!("1. [{}] the first one", theme::tick()))),
            "{drawn:#?}"
        );
        assert!(
            drawn
                .iter()
                .any(|row| row.contains("2. [ ] the second one")),
            "{drawn:#?}"
        );
        assert!(
            drawn.iter().any(|row| row.contains(&format!("3. {WORDS}")))
                && drawn.iter().any(|row| row.contains(&format!("4. {CHAT}"))),
            "{drawn:#?}"
        );
    }

    #[test]
    fn the_submit_tab_says_how_many_questions_are_still_open() {
        let questions = three();
        let (mut form, mut focus) = (Form::default(), 0);
        press(
            &mut form,
            &mut focus,
            &questions,
            &[KeyCode::Enter, KeyCode::Tab, KeyCode::Tab],
        );
        let some = drawn(&form, focus, &questions, 80);
        assert!(
            some.iter().any(|row| row == "2 questions are still open."),
            "{some:#?}"
        );
        let (mut form, mut focus) = (Form::default(), 0);
        press(
            &mut form,
            &mut focus,
            &questions,
            &[KeyCode::Enter, KeyCode::Enter, KeyCode::Enter],
        );
        let all = drawn(&form, focus, &questions, 80);
        assert!(
            all.iter().any(|row| row == "Send the answers.")
                && all.iter().any(|row| row.contains(&format!("1. {SUBMIT}"))),
            "{all:#?}"
        );
    }
}
