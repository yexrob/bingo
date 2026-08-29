//! The open interaction, answered. Options come from `interaction.answers` —
//! exactly what the kernel will accept — so a row can never promise something
//! the kernel would refuse.
//!
//! Answering is a write and being told is a frame: between the two the
//! interaction is still in `state.interactions`, so the dialog stays on screen
//! with a `…` marker until `InteractionResolved` removes it.

use bingo_sdk::{
    Activation, Answer, AnswerSpec, Interaction, InteractionId, InteractionKind, LoginFlow,
    QuestionOption,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

use crate::clock::Now;
use crate::composer::Composer;
use crate::effect::Effect;
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
        InteractionKind::Question { options, multi, .. } => {
            question_options(interaction, options, *multi)
        }
        InteractionKind::Confirm { .. } => interaction
            .answers
            .iter()
            .filter_map(|spec| match spec {
                AnswerSpec::Confirm => Some(plain("Confirm", Choice::Send(Answer::Confirm))),
                AnswerSpec::Cancel => Some(plain("Cancel", Choice::Send(Answer::Cancel))),
                _ => None,
            })
            .collect(),
        InteractionKind::Login { .. } => Vec::new(),
    }
}

fn permission_options(interaction: &Interaction, scope: Option<&str>) -> Vec<Opt> {
    let mut out = Vec::new();
    let offers = |spec| interaction.answers.contains(&spec);
    if offers(AnswerSpec::AllowOnce) {
        out.push(plain("Yes", Choice::Send(Answer::AllowOnce)));
    }
    if let Some(scope) = scope.filter(|_| offers(AnswerSpec::AllowSession)) {
        out.push(plain(
            &format!("Yes, for this session ({scope})"),
            Choice::Send(Answer::AllowSession {
                scope: scope.to_string(),
            }),
        ));
    }
    if offers(AnswerSpec::Deny) {
        out.push(plain("No, and tell it what to do instead", Choice::Words));
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

/// The dialog as lines. `width` bounds nothing here; the caller wraps.
pub fn lines(dialog: &Dialog, interaction: &Interaction) -> Vec<Line<'static>> {
    let mut out = vec![title(interaction)];
    out.extend(body(dialog, interaction));
    let options = options(interaction);
    if !options.is_empty() {
        out.push(Line::default());
    }
    for (index, option) in options.iter().enumerate() {
        out.extend(option_lines(dialog, index, option));
    }
    out.push(Line::from(Span::styled(
        format!("  {}", hint(dialog, interaction)),
        theme::dim(),
    )));
    out
}

fn title(interaction: &Interaction) -> Line<'static> {
    let (kind, name) = match &interaction.kind {
        InteractionKind::Permission { tool, .. } => ("Permission", tool.clone()),
        InteractionKind::Question { header, .. } => {
            ("Question", header.clone().unwrap_or_default())
        }
        InteractionKind::Confirm { title, .. } => ("Confirm", title.clone()),
        InteractionKind::Login { provider, .. } => ("Sign in", provider.clone()),
    };
    let mut spans = vec![Span::styled(
        kind.to_string(),
        theme::accent().patch(theme::bold()),
    )];
    if !name.is_empty() {
        spans.push(Span::styled(format!(" · {name}"), theme::bold()));
    }
    Line::from(spans)
}

fn body(dialog: &Dialog, interaction: &Interaction) -> Vec<Line<'static>> {
    match &interaction.kind {
        InteractionKind::Permission {
            summary, preview, ..
        } => {
            let mut out = vec![indented(summary)];
            if let Some(preview) = preview {
                let (rows, hidden) = preview::lines(preview, dialog.expanded);
                out.push(Line::default());
                out.extend(rows.into_iter().map(indent));
                if hidden > 0 {
                    out.push(Line::from(Span::styled(
                        format!("  … {hidden} more lines"),
                        theme::dim(),
                    )));
                }
            }
            out
        }
        InteractionKind::Question { question, .. } => vec![indented(question)],
        InteractionKind::Confirm { detail, .. } => vec![indented(detail)],
        InteractionKind::Login { flow, .. } => login_lines(flow),
    }
}

fn login_lines(flow: &LoginFlow) -> Vec<Line<'static>> {
    match flow {
        LoginFlow::Browser { url } => vec![indented(url)],
        LoginFlow::Device { url, code } => vec![indented(url), indented(&format!("code: {code}"))],
        LoginFlow::Paste => vec![indented("paste the credential")],
    }
}

fn option_lines(dialog: &Dialog, index: usize, option: &Opt) -> Vec<Line<'static>> {
    let focused = index == dialog.focus;
    let style = if focused {
        theme::accent()
    } else {
        theme::dim()
    };
    let mark = match &option.choice {
        Choice::Toggle(id) if dialog.chosen.iter().any(|c| c == id) => "[x] ",
        Choice::Toggle(_) => "[ ] ",
        _ => "",
    };
    let mut out = vec![Line::from(vec![
        Span::styled(if focused { "❯ " } else { "  " }, style),
        Span::styled(format!("{}. {mark}{}", index + 1, option.label), style),
    ])];
    if let Some(description) = &option.description {
        out.push(Line::from(Span::styled(
            format!("     {description}"),
            theme::dim(),
        )));
    }
    if let Some(words) = dialog.words.as_ref().filter(|_| focused) {
        out.push(Line::from(vec![
            Span::raw("     > "),
            Span::raw(words.text().to_string()),
        ]));
    }
    out
}

fn hint(dialog: &Dialog, interaction: &Interaction) -> String {
    if dialog.answered {
        return "… waiting for the kernel".to_string();
    }
    let leave = match interaction.kind {
        InteractionKind::Permission { .. } => "esc to deny",
        _ => "esc to cancel",
    };
    let mut parts = if dialog.words.is_some() {
        vec!["enter to send"]
    } else {
        vec!["enter to select", "↑/↓ to navigate"]
    };
    if has_preview(interaction) {
        parts.push(if dialog.expanded {
            "ctrl+e to collapse"
        } else {
            "ctrl+e to expand"
        });
    }
    parts.push(leave);
    parts.join(" · ")
}

fn has_preview(interaction: &Interaction) -> bool {
    matches!(
        &interaction.kind,
        InteractionKind::Permission {
            preview: Some(_),
            ..
        }
    )
}

fn indented(text: &str) -> Line<'static> {
    Line::from(Span::raw(format!("  {text}")))
}

fn indent(line: Line<'static>) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    spans.extend(line.spans);
    Line::from(spans)
}
