//! The open interaction, answered. Options come from `interaction.answers` —
//! exactly what the kernel will accept — so a row can never promise something
//! the kernel would refuse.
//!
//! Answering is a write and being told is a frame: between the two the
//! interaction is still in `state.interactions`, so the dialog stays on screen
//! with a `…` marker until `InteractionResolved` removes it.

use bingo_sdk::{
    Activation, Answer, AnswerSpec, Interaction, InteractionId, InteractionKind, LoginFlow,
    Preview, Question, QuestionOption,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::clock::Now;
use crate::composer::Composer;
use crate::effect::Effect;
use crate::form::{self, Form};
use crate::{preview, theme};

/// What one row of the dialog does when it is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// A complete answer, sent as it is.
    Send(Answer),
    /// Opens the row where the person says what to do instead.
    Words,
    /// One member of a multiple-choice set.
    Toggle(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opt {
    pub label: String,
    pub description: Option<String>,
    pub choice: Choice,
}

/// The surface's own state for the interaction on top of the stack.
#[derive(Clone, Debug, Default)]
pub struct Dialog {
    /// Whose state this is; a different interaction resets everything.
    current: Option<InteractionId>,
    pub focus: usize,
    pub expanded: bool,
    /// The feedback or free-text row, open once the answer needs words.
    pub words: Option<Composer>,
    pub chosen: Vec<String>,
    /// Answered, waiting for the frame that closes it.
    pub answered: bool,
    /// A form's own state: which question is on screen, and what each of the
    /// others was left on. `focus` is still the cursor of the one on screen,
    /// so every key, click and window here works on a form unchanged.
    pub form: Form,
}

impl Dialog {
    /// Point the dialog at whatever is on top now, forgetting the last one.
    pub fn focus_on(&mut self, interaction: Option<&Interaction>) {
        let id = interaction.map(|i| i.id.clone());
        if id != self.current {
            *self = Self {
                current: id,
                ..Self::default()
            };
        }
    }

    pub fn on_key(&mut self, interaction: &Interaction, key: KeyEvent, now: Now) -> Vec<Effect> {
        if self.answered || guarded(interaction, now) {
            return Vec::new();
        }
        if key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.expanded = !self.expanded;
            return Vec::new();
        }
        if key.code == KeyCode::Esc {
            return self.send(interaction, cancel(interaction));
        }
        if let InteractionKind::Form { questions } = &interaction.kind {
            let answer = form::on_key(&mut self.form, &mut self.focus, questions, key);
            return self.send(interaction, answer);
        }
        if self.words.is_some() {
            let done = self
                .words
                .as_mut()
                .is_some_and(|words| words_key(words, key));
            return self.typing(interaction, done);
        }
        self.choosing(interaction, key)
    }

    fn typing(&mut self, interaction: &Interaction, done: bool) -> Vec<Effect> {
        if !done {
            return Vec::new();
        }
        let text = self
            .words
            .take()
            .map(|w| w.text().to_string())
            .unwrap_or_default();
        self.send(interaction, Some(words_answer(interaction, text)))
    }

    fn choosing(&mut self, interaction: &Interaction, key: KeyEvent) -> Vec<Effect> {
        let options = options(interaction);
        match key.code {
            KeyCode::Up => self.focus = self.focus.saturating_sub(1),
            KeyCode::Down => self.focus = (self.focus + 1).min(options.len().saturating_sub(1)),
            KeyCode::Char(' ') => return self.toggle(&options),
            KeyCode::Enter => return self.confirm(interaction, &options),
            KeyCode::Char(c @ '1'..='9') if typed(key) => {
                let Some(index) = digit(c, options.len()) else {
                    return Vec::new();
                };
                self.focus = index;
                return self.confirm(interaction, &options);
            }
            KeyCode::Char(c) if typed(key) => {
                let Some(index) = letter(c, &options) else {
                    return Vec::new();
                };
                self.focus = index;
                return self.confirm(interaction, &options);
            }
            _ => {}
        }
        Vec::new()
    }

    fn toggle(&mut self, options: &[Opt]) -> Vec<Effect> {
        let Some(Choice::Toggle(id)) = options.get(self.focus).map(|o| &o.choice) else {
            return Vec::new();
        };
        match self.chosen.iter().position(|c| c == id) {
            Some(i) => {
                self.chosen.remove(i);
            }
            None => self.chosen.push(id.clone()),
        }
        Vec::new()
    }

    /// Enter on the focused row. In a multiple-choice question every row but
    /// the free-text one confirms the whole set.
    fn confirm(&mut self, interaction: &Interaction, options: &[Opt]) -> Vec<Effect> {
        match options.get(self.focus).map(|o| o.choice.clone()) {
            Some(Choice::Send(answer)) => self.send(interaction, Some(answer)),
            Some(Choice::Words) => {
                self.words = Some(Composer::default());
                Vec::new()
            }
            Some(Choice::Toggle(_)) => self.send(
                interaction,
                Some(Answer::Choice {
                    ids: self.chosen.clone(),
                }),
            ),
            None => Vec::new(),
        }
    }

    fn send(&mut self, interaction: &Interaction, answer: Option<Answer>) -> Vec<Effect> {
        let Some(answer) = answer else {
            return Vec::new();
        };
        self.answered = true;
        vec![Effect::Answer {
            interaction: interaction.id.clone(),
            answer,
            activation: Activation::Keyboard,
        }]
    }
}

/// Keyboard answers before the guard are refused by the kernel, so the dialog
/// does not send them.
fn guarded(interaction: &Interaction, now: Now) -> bool {
    interaction
        .guard_until
        .is_some_and(|until| now.wall < until)
}

/// Edit the words row; `true` when the person is done with it.
fn words_key(words: &mut Composer, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => return true,
        KeyCode::Backspace => words.backspace(),
        KeyCode::Left => words.left(),
        KeyCode::Right => words.right(),
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            words.insert(&c.to_string())
        }
        _ => {}
    }
    false
}

fn words_answer(interaction: &Interaction, text: String) -> Answer {
    let text = text.trim().to_string();
    match interaction.kind {
        InteractionKind::Permission { .. } => Answer::Deny {
            feedback: (!text.is_empty()).then_some(text),
        },
        _ => Answer::Text { text },
    }
}

/// Leaving a permission prompt is the refusal; leaving anything else cancels.
fn cancel(interaction: &Interaction) -> Option<Answer> {
    if interaction.answers.contains(&AnswerSpec::Deny) {
        return Some(Answer::Deny { feedback: None });
    }
    interaction
        .answers
        .contains(&AnswerSpec::Cancel)
        .then_some(Answer::Cancel)
}

/// A bare letter or digit, not a chord.
fn typed(key: KeyEvent) -> bool {
    (key.modifiers - KeyModifiers::SHIFT).is_empty()
}

fn digit(c: char, len: usize) -> Option<usize> {
    let index = (c as usize) - ('1' as usize);
    (index < len).then_some(index)
}

/// `y`, `a` and `n` name the three permission answers wherever they sit.
fn letter(c: char, options: &[Opt]) -> Option<usize> {
    let wanted = match c {
        'y' => AnswerSpec::AllowOnce,
        'a' => AnswerSpec::AllowSession,
        'n' => AnswerSpec::Deny,
        _ => return None,
    };
    options.iter().position(|o| match &o.choice {
        Choice::Send(answer) => answer.spec() == wanted,
        Choice::Words => wanted == AnswerSpec::Deny,
        Choice::Toggle(_) => false,
    })
}

/// The rows this interaction offers, in the order they are shown.
pub fn options(interaction: &Interaction) -> Vec<Opt> {
    match &interaction.kind {
        InteractionKind::Permission { session_scope, .. } => {
            permission_options(interaction, session_scope.as_deref())
        }
        InteractionKind::Question(Question { options, multi, .. }) => {
            question_options(interaction, options, *multi)
        }
        // The form has a card of its own (M53); the dialog answers no key of
        // it, so `esc` is the only way out of one drawn here.
        InteractionKind::Form { .. } => Vec::new(),
        InteractionKind::Confirm { .. } => interaction
            .answers
            .iter()
            .filter_map(|spec| match spec {
                AnswerSpec::Confirm => Some(plain("Confirm", Choice::Send(Answer::Confirm))),
                AnswerSpec::Cancel => Some(plain("Cancel (esc)", Choice::Send(Answer::Cancel))),
                _ => None,
            })
            .collect(),
        InteractionKind::Login { flow, .. } => login_options(interaction, flow),
    }
}

/// A browser or device flow finishes on its own; the one row is the way
/// out. A paste flow opens the words row for the credential.
fn login_options(interaction: &Interaction, flow: &LoginFlow) -> Vec<Opt> {
    let mut out = Vec::new();
    if matches!(flow, LoginFlow::Paste) && interaction.answers.contains(&AnswerSpec::Text) {
        out.push(plain("Paste it here", Choice::Words));
    }
    if interaction.answers.contains(&AnswerSpec::Cancel) {
        out.push(plain("Cancel (esc)", Choice::Send(Answer::Cancel)));
    }
    out
}

fn permission_options(interaction: &Interaction, scope: Option<&str>) -> Vec<Opt> {
    let mut out = Vec::new();
    let offers = |spec| interaction.answers.contains(&spec);
    if offers(AnswerSpec::AllowOnce) {
        out.push(plain("Yes", Choice::Send(Answer::AllowOnce)));
    }
    if let Some(scope) = scope.filter(|_| offers(AnswerSpec::AllowSession)) {
        out.push(plain(
            &format!("Yes, allow {scope} during this session"),
            Choice::Send(Answer::AllowSession {
                scope: scope.to_string(),
            }),
        ));
    }
    if offers(AnswerSpec::Deny) {
        out.push(plain(
            "No, and tell bingo what to do differently (esc)",
            Choice::Words,
        ));
    }
    out
}

fn question_options(
    interaction: &Interaction,
    options: &[QuestionOption],
    multi: bool,
) -> Vec<Opt> {
    let mut out: Vec<Opt> = options
        .iter()
        .map(|option| Opt {
            label: option.label.clone(),
            description: option.description.clone(),
            choice: if multi {
                Choice::Toggle(option.id.clone())
            } else {
                Choice::Send(Answer::Choice {
                    ids: vec![option.id.clone()],
                })
            },
        })
        .collect();
    if interaction.answers.contains(&AnswerSpec::Text) {
        out.push(plain("Other", Choice::Words));
    }
    out
}

fn plain(label: &str, choice: Choice) -> Opt {
    Opt {
        label: label.to_string(),
        description: None,
        choice,
    }
}

/// The card, as the lines inside it (design §4): a bold title, what it is
/// about on its own tints, the one line that asks, then the answers. The box
/// around it, the dimming behind it and its reveal are the frame's.
///
/// `width` bounds nothing here; the caller wraps. `agent` names the
/// sub-session that asked, when it was not the one on screen (ADR-0010 §3);
/// `cwd` is that session's directory, which is what makes a path short.
///
/// Every row comes with the option it belongs to, so one walk lays the card
/// out and a click lands where the eye is.
pub fn rows(
    dialog: &Dialog,
    interaction: &Interaction,
    agent: Option<&str>,
    cwd: &str,
    width: usize,
) -> Vec<(Line<'static>, Option<usize>)> {
    let mut out = vec![(title(dialog, interaction, agent), None)];
    if let InteractionKind::Form { questions } = &interaction.kind {
        out.extend(form::rows(&dialog.form, dialog.focus, questions, width));
        out.extend(answering(dialog));
        return out;
    }
    out.extend(
        body(dialog, interaction)
            .into_iter()
            .map(|line| (line, None)),
    );
    if let Some(question) = question(interaction, cwd) {
        out.push((Line::default(), None));
        out.push((question, None));
    }
    for (index, option) in options(interaction).iter().enumerate() {
        out.extend(
            option_lines(dialog, index, option)
                .into_iter()
                .map(|line| (line, Some(index))),
        );
    }
    out.extend(answering(dialog));
    out
}

/// The mark of an answer already sent: it stays on screen until the frame that
/// closes the card arrives.
fn answering(dialog: &Dialog) -> Vec<(Line<'static>, Option<usize>)> {
    if !dialog.answered {
        return Vec::new();
    }
    vec![(
        Line::from(Span::styled(
            format!("  {} waiting for the kernel", theme::ellipsis()),
            theme::dim(),
        )),
        None,
    )]
}

/// What kind of card this is, in one word a person reads first — for a form,
/// the tab row, which is the one row that may never give way.
fn title(dialog: &Dialog, interaction: &Interaction, agent: Option<&str>) -> Line<'static> {
    let mut spans = match &interaction.kind {
        InteractionKind::Form { questions } => form::tabs(&dialog.form, questions),
        _ => vec![Span::styled(named(interaction), theme::bold())],
    };
    if let Some(agent) = agent {
        spans.push(Span::styled(format!(" · {agent}"), theme::presence()));
    }
    Line::from(spans)
}

fn named(interaction: &Interaction) -> String {
    match &interaction.kind {
        InteractionKind::Permission { tool, .. } => tool.clone(),
        InteractionKind::Question(Question { header, .. }) => {
            header.clone().unwrap_or_else(|| "Question".to_string())
        }
        InteractionKind::Confirm { title, .. } => title.clone(),
        InteractionKind::Login { provider, .. } => format!("Sign in to {provider}"),
        InteractionKind::Form { .. } => "Questions".to_string(),
    }
}

/// The one line that asks. A permission asks about the call the kernel
/// summarised; a question and a confirmation carry their own words.
fn question(interaction: &Interaction, cwd: &str) -> Option<Line<'static>> {
    let text = match &interaction.kind {
        InteractionKind::Permission { summary, .. } => {
            format!("Do you want to {}?", opening(&shorten(summary, cwd)))
        }
        InteractionKind::Question(Question { question, .. }) => question.clone(),
        InteractionKind::Form { .. } => return None,
        InteractionKind::Confirm { detail, .. } => detail.clone(),
        InteractionKind::Login { .. } => return None,
    };
    Some(Line::from(Span::styled(text, theme::text())))
}

/// The kernel's summary opens a sentence here, so its first letter joins it.
fn opening(summary: &str) -> String {
    let mut chars = summary.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn shorten(text: &str, cwd: &str) -> String {
    crate::paths::shorten_in(text, cwd, crate::paths::home())
}

fn body(dialog: &Dialog, interaction: &Interaction) -> Vec<Line<'static>> {
    match &interaction.kind {
        InteractionKind::Permission {
            preview: Some(preview),
            ..
        } => preview_lines(preview, dialog.expanded),
        InteractionKind::Login { flow, .. } => login_lines(flow),
        _ => Vec::new(),
    }
}

/// The diff or the command, on the tints of what it does.
fn preview_lines(preview: &Preview, expanded: bool) -> Vec<Line<'static>> {
    let (rows, hidden) = preview::lines(preview, expanded);
    let mut out: Vec<Line<'static>> = rows.into_iter().map(indent).collect();
    if hidden > 0 {
        out.push(Line::from(Span::styled(
            format!("  {} +{hidden} lines (ctrl+e to expand)", theme::ellipsis()),
            theme::dim(),
        )));
    }
    out
}

fn login_lines(flow: &LoginFlow) -> Vec<Line<'static>> {
    match flow {
        LoginFlow::Browser { url } => vec![
            indented("Finish in the browser. If it did not open, go to:"),
            link(url),
        ],
        LoginFlow::Device { url, code } => vec![
            indented("Open this address and enter the code:"),
            link(url),
            indented(&format!("code: {code}")),
        ],
        LoginFlow::Paste => vec![indented("A credential minted elsewhere.")],
    }
}

fn option_lines(dialog: &Dialog, index: usize, option: &Opt) -> Vec<Line<'static>> {
    let focused = index == dialog.focus;
    let style = if focused { theme::text() } else { theme::dim() };
    let mut out = vec![Line::from(vec![
        theme::cursor_span(focused),
        Span::styled(
            format!(
                "{}. {}{}",
                index + 1,
                box_mark(dialog, option),
                option.label
            ),
            style,
        ),
    ])];
    if let Some(description) = &option.description {
        out.push(Line::from(Span::styled(
            format!("     {description}"),
            theme::dim(),
        )));
    }
    if let Some(words) = dialog.words.as_ref().filter(|_| focused) {
        out.push(Line::from(vec![
            Span::styled(format!("     {} ", theme::user()), theme::dim()),
            Span::styled(words.text().to_string(), theme::text()),
        ]));
    }
    out
}

/// One of a set is a box that is ticked or not; one of a choice is neither.
fn box_mark(dialog: &Dialog, option: &Opt) -> String {
    match &option.choice {
        Choice::Toggle(id) => format!("{} ", theme::todo(dialog.chosen.iter().any(|c| c == id))),
        _ => String::new(),
    }
}

fn indented(text: &str) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), theme::text()))
}

fn link(url: &str) -> Line<'static> {
    Line::from(Span::styled(format!("  {url}"), theme::link()))
}

fn indent(line: Line<'static>) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    spans.extend(line.spans);
    Line::from(spans)
}
