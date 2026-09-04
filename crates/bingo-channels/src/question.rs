//! One question, two rungs (ADR-0016 §3).
//!
//! A `Question` is the kernel's `Interaction` laid out for a chat: a prompt, a
//! numbered set of choices, and whether words of one's own count. Native
//! buttons and a numbered list are two renderings of the same ladder — the key
//! a button carries is the number a person types — so an answer means the same
//! thing whichever rung it arrives on, and the kernel's single-winner rule
//! needs nothing new.

use bingo_sdk::{
    Answer, AnswerSpec, CancelReason, Interaction, InteractionId, InteractionKind, ResolvedBy,
};

use crate::limits::Limits;

/// One way to answer: what a button carries, what a person reads, and the
/// answer either produces.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    /// `1`..`n`: the button's value and the reply that picks it.
    pub key: String,
    pub label: String,
    pub answer: Answer,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    pub id: InteractionId,
    pub prompt: String,
    pub choices: Vec<Choice>,
    /// Words of one's own are an answer too.
    pub free_text: bool,
}

/// The interaction as a chat can answer it, or `None` when this surface has
/// no rung for it — a browser login is nobody's chat message.
pub fn ladder(interaction: &Interaction) -> Option<Question> {
    let offered = |spec: AnswerSpec| interaction.answers.contains(&spec);
    let (prompt, mut answers) = match &interaction.kind {
        InteractionKind::Permission {
            tool,
            summary,
            session_scope,
            ..
        } => (
            format!("{tool}: {summary}"),
            permission(session_scope.as_deref(), &offered),
        ),
        InteractionKind::Question(bingo_sdk::Question {
            question,
            header,
            options,
            ..
        }) => (
            match header {
                Some(header) => format!("{header}: {question}"),
                None => question.clone(),
            },
            options
                .iter()
                .map(|option| {
                    (
                        option.label.clone(),
                        Answer::Choice {
                            ids: vec![option.id.clone()],
                        },
                    )
                })
                .collect(),
        ),
        InteractionKind::Confirm { title, detail } => (
            format!("{title}\n{detail}"),
            vec![
                ("Yes".to_string(), Answer::Confirm),
                ("No".to_string(), Answer::Cancel),
            ],
        ),
        // A form is asked one message per question (M53), which is not one
        // ladder; `form` builds those.
        InteractionKind::Form { .. } | InteractionKind::Login { .. } => return None,
    };
    answers.retain(|(_, answer)| offered(answer.spec()));
    if answers.is_empty() && !offered(AnswerSpec::Text) {
        return None;
    }
    Some(Question {
        id: interaction.id.clone(),
        prompt,
        choices: numbered(answers),
        free_text: offered(AnswerSpec::Text),
    })
}

/// The permission rungs, widest first. `AllowSession` without a scope would
/// install no rule, so it is not offered as if it would.
fn permission(
    session_scope: Option<&str>,
    offered: &impl Fn(AnswerSpec) -> bool,
) -> Vec<(String, Answer)> {
    let mut answers = vec![("Allow once".to_string(), Answer::AllowOnce)];
    if let Some(scope) = session_scope.filter(|_| offered(AnswerSpec::AllowSession)) {
        answers.push((
            format!("Allow {scope} for this session"),
            Answer::AllowSession {
                scope: scope.to_string(),
            },
        ));
    }
    answers.push(("Deny".to_string(), Answer::Deny { feedback: None }));
    answers
}

fn numbered(answers: Vec<(String, Answer)>) -> Vec<Choice> {
    answers
        .into_iter()
        .enumerate()
        .map(|(index, (label, answer))| Choice {
            key: (index + 1).to_string(),
            label,
            answer,
        })
        .collect()
}

impl Question {
    /// The choices as buttons, or `None` when there are more than the platform
    /// will draw — in which case the whole question drops to the lower rung
    /// rather than showing a person half of their options.
    pub fn buttons(&self, limits: &Limits) -> Option<Vec<Choice>> {
        if self.choices.is_empty() || self.choices.len() > limits.max_actions {
            return None;
        }
        Some(
            self.choices
                .iter()
                .map(|choice| Choice {
                    label: limits.label(&choice.label),
                    ..choice.clone()
                })
                .collect(),
        )
    }

    /// The lower rung: the question, its choices numbered, and how to answer.
    pub fn numbered(&self) -> String {
        let mut text = self.prompt.clone();
        for choice in &self.choices {
            text.push_str(&format!("\n{}. {}", choice.key, choice.label));
        }
        text.push_str(&format!("\n\n{}", self.how()));
        text
    }

    fn how(&self) -> &'static str {
        match (self.choices.is_empty(), self.free_text) {
            (true, _) => "Reply in your own words.",
            (false, true) => "Reply with a number, the words above, or your own.",
            (false, false) => "Reply with a number or the words above.",
        }
    }

    /// What a reply means, or `None` when it means nothing here — in which
    /// case it is a new prompt, not a wrong answer.
    pub fn parse(&self, reply: &str) -> Option<Answer> {
        let reply = reply.trim();
        if reply.is_empty() {
            return None;
        }
        let picked = self
            .choices
            .iter()
            .find(|choice| choice.key == reply || choice.label.eq_ignore_ascii_case(reply));
        match (picked, self.free_text) {
            (Some(choice), _) => Some(choice.answer.clone()),
            (None, true) => Some(Answer::Text {
                text: reply.to_string(),
            }),
            (None, false) => None,
        }
    }

    /// What a button carries back, by its key.
    pub fn pick(&self, key: &str) -> Option<Answer> {
        self.choices
            .iter()
            .find(|choice| choice.key == key)
            .map(|choice| choice.answer.clone())
    }

    /// The line that replaces the buttons once the question is settled:
    /// what was decided, and where (ADR-0016 §3). `here` is this surface's
    /// own client name, so a decision made in this chat says so plainly.
    pub fn outcome(&self, answer: &Answer, by: &ResolvedBy, here: &str) -> String {
        format!("{}{}", self.decided(answer), place(by, here))
    }

    fn decided(&self, answer: &Answer) -> String {
        match answer {
            Answer::AllowOnce => "approved".into(),
            Answer::AllowSession { .. } => "approved for this session".into(),
            Answer::Deny { .. } => "denied".into(),
            Answer::Confirm => "confirmed".into(),
            Answer::Cancel => "cancelled".into(),
            Answer::Text { .. } | Answer::Form { .. } => "answered".into(),
            Answer::Choice { ids } => match self.labels(ids) {
                labels if labels.is_empty() => "answered".into(),
                labels => format!("chose {}", labels.join(", ")),
            },
        }
    }

    /// The labels this question showed for those option ids: the answer
    /// carries ids, and a person reads labels.
    fn labels(&self, ids: &[String]) -> Vec<String> {
        self.choices
            .iter()
            .filter(|choice| match &choice.answer {
                Answer::Choice { ids: chosen } => chosen.iter().any(|id| ids.contains(id)),
                _ => false,
            })
            .map(|choice| choice.label.clone())
            .collect()
    }
}

/// Where a resolution came from, as a person in this chat reads it.
fn place(by: &ResolvedBy, here: &str) -> String {
    match by {
        ResolvedBy::Client { name, .. } if name == here => String::new(),
        ResolvedBy::Client { surface, .. } => format!(" in {}", surface_name(surface)),
        ResolvedBy::Kernel => " by the session".into(),
        ResolvedBy::Policy => " by a rule".into(),
    }
}

/// The surfaces a person has a name for. This names no plugin and imports
/// none: it is the same string the frame carries, spelled the way it is said.
fn surface_name(surface: &str) -> String {
    match surface {
        "tui" => "the TUI".into(),
        "print" => "the terminal".into(),
        "rpc" => "another client".into(),
        "channels" => "another chat".into(),
        other => other.to_string(),
    }
}

/// A question nobody will answer any more, and why. The buttons come off for
/// this as surely as for an answer: no live button outlives its question.
pub fn withdrawn(reason: &CancelReason) -> String {
    let why = match reason {
        CancelReason::TurnEnded => "the turn ended",
        CancelReason::CommandEnded => "the command ended",
        CancelReason::Interrupted => "it was interrupted",
        CancelReason::SessionClosed => "the session closed",
        CancelReason::Expired => "it expired",
        CancelReason::Superseded => "another question replaced it",
    };
    format!("withdrawn: {why}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::{Dialect, Encoding};
    use bingo_sdk::{Preview, QuestionOption, SessionId};
    use jiff::Timestamp;

    fn limits() -> Limits {
        Limits {
            max_text: (4000, Encoding::Chars),
            dialect: Dialect::Markdown,
            max_actions: 3,
            max_label: 20,
        }
    }

    fn interaction(kind: InteractionKind, answers: Vec<AnswerSpec>) -> Interaction {
        Interaction {
            id: InteractionId::from_raw("int_1"),
            session: SessionId::from_raw("ses_1"),
            turn: None,
            item: None,
            opened_at: Timestamp::from_second(1_700_000_000).unwrap(),
            guard_until: None,
            expires_at: None,
            kind,
            answers,
        }
    }

    fn permission_interaction(scope: Option<&str>) -> Interaction {
        interaction(
            InteractionKind::Permission {
                tool: "Bash".into(),
                summary: "run `cargo test`".into(),
                preview: Some(Preview::Command {
                    command: "cargo test".into(),
                    cwd: "/tmp".into(),
                }),
                session_scope: scope.map(str::to_owned),
            },
            vec![
                AnswerSpec::AllowOnce,
                AnswerSpec::AllowSession,
                AnswerSpec::Deny,
            ],
        )
    }

    #[test]
    fn a_permission_offers_the_three_rungs_and_numbers_them() {
        let question = ladder(&permission_interaction(Some("Bash(cargo test:*)"))).unwrap();
        assert_eq!(question.prompt, "Bash: run `cargo test`");
        assert_eq!(
            question
                .choices
                .iter()
                .map(|c| (c.key.as_str(), c.label.as_str()))
                .collect::<Vec<_>>(),
            [
                ("1", "Allow once"),
                ("2", "Allow Bash(cargo test:*) for this session"),
                ("3", "Deny"),
            ]
        );
        assert!(!question.free_text);
    }

    #[test]
    fn a_session_rule_nobody_offered_is_not_shown_as_if_it_would_be_installed() {
        let question = ladder(&permission_interaction(None)).unwrap();
        assert_eq!(question.choices.len(), 2, "{:?}", question.choices);
        assert_eq!(question.choices[1].key, "2");
    }

    #[test]
    fn the_same_key_answers_on_either_rung() {
        let question = ladder(&permission_interaction(None)).unwrap();
        assert_eq!(question.parse("1"), Some(Answer::AllowOnce));
        assert_eq!(question.pick("1"), Some(Answer::AllowOnce));
        assert_eq!(
            question.parse("deny"),
            Some(Answer::Deny { feedback: None }),
            "the words work as well as the number"
        );
        assert_eq!(
            question.parse("maybe"),
            None,
            "anything else is a new prompt"
        );
    }

    #[test]
    fn a_numbered_list_says_how_to_answer_it() {
        let question = ladder(&permission_interaction(None)).unwrap();
        assert_eq!(
            question.numbered(),
            "Bash: run `cargo test`\n1. Allow once\n2. Deny\n\n\
             Reply with a number or the words above."
        );
    }

    #[test]
    fn more_choices_than_the_platform_draws_drops_the_whole_question_to_the_lower_rung() {
        let question = ladder(&interaction(
            InteractionKind::Question(bingo_sdk::Question {
                question: "Which file?".into(),
                header: None,
                options: ["a", "b", "c", "d"]
                    .iter()
                    .map(|id| QuestionOption {
                        id: (*id).into(),
                        label: format!("file {id}"),
                        description: None,
                        role: None,
                        preview: None,
                    })
                    .collect(),
                free_text: false,
                multi: false,
            }),
            vec![AnswerSpec::Choice],
        ))
        .unwrap();
        assert!(
            question.buttons(&limits()).is_none(),
            "four will not fit in three"
        );
        assert!(question.numbered().contains("4. file d"));
    }

    #[test]
    fn a_button_label_is_cut_to_the_platforms_length() {
        let question = ladder(&permission_interaction(Some("Bash(cargo test:*)"))).unwrap();
        let buttons = question.buttons(&limits()).unwrap();
        assert_eq!(buttons[1].label, "Allow Bash(cargo te…");
        assert_eq!(buttons[1].key, "2", "the key is not cut with the label");
    }

    #[test]
    fn free_text_is_accepted_where_the_spec_allows_it() {
        let question = ladder(&interaction(
            InteractionKind::Question(bingo_sdk::Question {
                question: "What should it be called?".into(),
                header: Some("Name".into()),
                options: vec![],
                free_text: true,
                multi: false,
            }),
            vec![AnswerSpec::Text],
        ))
        .unwrap();
        assert_eq!(question.prompt, "Name: What should it be called?");
        assert_eq!(
            question.parse("bingo"),
            Some(Answer::Text {
                text: "bingo".into()
            })
        );
        assert!(question.numbered().ends_with("Reply in your own words."));
    }

    #[test]
    fn a_login_has_no_rung_in_a_chat() {
        assert!(
            ladder(&interaction(
                InteractionKind::Login {
                    provider: "codex".into(),
                    flow: bingo_sdk::LoginFlow::Paste,
                },
                vec![AnswerSpec::Text, AnswerSpec::Cancel],
            ))
            .is_none()
        );
    }

    #[test]
    fn an_outcome_names_the_decision_and_where_it_was_made() {
        let question = ladder(&permission_interaction(None)).unwrap();
        let tui = ResolvedBy::Client {
            name: "tui".into(),
            surface: "tui".into(),
        };
        assert_eq!(
            question.outcome(&Answer::AllowOnce, &tui, "loopback/oc_1"),
            "approved in the TUI"
        );
        assert_eq!(
            question.outcome(
                &Answer::Deny { feedback: None },
                &ResolvedBy::Client {
                    name: "loopback/oc_1".into(),
                    surface: "channels".into(),
                },
                "loopback/oc_1"
            ),
            "denied",
            "a decision made here needs no elsewhere"
        );
        assert_eq!(
            question.outcome(&Answer::AllowOnce, &ResolvedBy::Policy, "loopback/oc_1"),
            "approved by a rule"
        );
    }

    #[test]
    fn a_chosen_option_is_named_by_the_label_the_chat_showed() {
        let question = ladder(&interaction(
            InteractionKind::Question(bingo_sdk::Question {
                question: "Which file?".into(),
                header: None,
                options: vec![QuestionOption {
                    id: "a".into(),
                    label: "Cargo.toml".into(),
                    description: None,
                    role: None,
                    preview: None,
                }],
                free_text: false,
                multi: false,
            }),
            vec![AnswerSpec::Choice],
        ))
        .unwrap();
        assert_eq!(
            question.outcome(
                &Answer::Choice {
                    ids: vec!["a".into()]
                },
                &ResolvedBy::Kernel,
                "here"
            ),
            "chose Cargo.toml by the session"
        );
    }

    #[test]
    fn a_withdrawn_question_says_why() {
        assert_eq!(
            withdrawn(&CancelReason::TurnEnded),
            "withdrawn: the turn ended"
        );
    }
}
