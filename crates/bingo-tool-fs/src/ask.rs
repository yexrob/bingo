//! `AskUserQuestion`: the model's questions, one interaction each. The kernel
//! opens and resolves them; the tool only asks and reports what came back.

use std::collections::HashSet;

use async_trait::async_trait;
use bingo_sdk::{
    Answer, AnswerSpec, InteractionKind, QuestionOption, Tool, ToolContext, ToolError, ToolOutput,
    ToolSpec, ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

const MAX_QUESTIONS: usize = 4;
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 4;

const DESCRIPTION: &str = "\
Ask the user a multiple-choice question, and wait for the answer. Use it only \
when the answer would change what you do next: a preference, or an ambiguity \
you cannot settle from the codebase yourself. When there is a sensible \
default, take it and say so instead of asking. One to four questions, each \
with two to four options; put the option you recommend first. Do not offer an \
\"Other\" option — the user can always type an answer of their own, or \
decline the question entirely. The result gives back what the user chose, one \
line per question.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskArgs {
    /// The questions to ask, one to four, put to the user one after another.
    pub questions: Vec<AskQuestion>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskQuestion {
    /// The question in full, clear and specific, ending with a question mark.
    pub question: String,
    /// A short tag naming the decision, e.g. `Auth method`.
    pub header: String,
    /// The answers to choose from: two to four, with distinct labels.
    pub options: Vec<AskOption>,
    /// Let the user choose more than one option.
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskOption {
    /// The answer itself, in a few words.
    pub label: String,
    /// What choosing it means, or where it leads.
    pub description: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AskUserQuestionTool;

/// Counts and distinctness the schema cannot state. A malformed set of
/// questions is refused whole, before the user is interrupted at all.
fn validate(args: &AskArgs) -> Result<(), ToolError> {
    let count = args.questions.len();
    if !(1..=MAX_QUESTIONS).contains(&count) {
        return Err(ToolError::InvalidInput(format!(
            "ask 1 to {MAX_QUESTIONS} questions at a time, not {count}"
        )));
    }
    let mut seen = HashSet::new();
    for question in &args.questions {
        if !seen.insert(question.question.trim()) {
            return Err(ToolError::InvalidInput(format!(
                "the question {:?} is asked twice",
                question.question
            )));
        }
        validate_options(question)?;
    }
    Ok(())
}

fn validate_options(question: &AskQuestion) -> Result<(), ToolError> {
    let count = question.options.len();
    if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&count) {
        return Err(ToolError::InvalidInput(format!(
            "{:?} needs {MIN_OPTIONS} to {MAX_OPTIONS} options, not {count}",
            question.question
        )));
    }
    let mut labels = HashSet::new();
    for option in &question.options {
        if !labels.insert(option.label.trim()) {
            return Err(ToolError::InvalidInput(format!(
                "the option {:?} is offered twice",
                option.label
            )));
        }
    }
    Ok(())
}

/// The interaction the kernel opens. The option's position is its id, so an
/// answer maps back to the label the model wrote.
fn interaction(question: &AskQuestion) -> InteractionKind {
    InteractionKind::Question {
        question: question.question.clone(),
        header: Some(question.header.clone()),
        options: question
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| QuestionOption {
                id: index.to_string(),
                label: option.label.clone(),
                description: option.description.clone(),
                // A model's question is a person's alone to answer.
                role: None,
            })
            .collect(),
        // A person may always answer in their own words, which is why the
        // model is told not to offer an "Other" option.
        free_text: true,
        multi: question.multi_select,
    }
}

/// The answers the kernel will accept, read off the question rather than
/// stated a second time beside it.
fn answer_specs(kind: &InteractionKind) -> Vec<AnswerSpec> {
    let mut specs = vec![AnswerSpec::Choice];
    if matches!(
        kind,
        InteractionKind::Question {
            free_text: true,
            ..
        }
    ) {
        specs.push(AnswerSpec::Text);
    }
    specs.push(AnswerSpec::Cancel);
    specs
}

/// The labels the chosen ids name, in the order the question listed them.
fn chosen(question: &AskQuestion, ids: &[String]) -> Vec<String> {
    question
        .options
        .iter()
        .enumerate()
        .filter(|(index, _)| ids.iter().any(|id| id == &index.to_string()))
        .map(|(_, option)| option.label.clone())
        .collect()
}

/// One answered question, as the model reads it back.
fn answered(question: &AskQuestion, answer: Answer) -> Result<String, ToolError> {
    match answer {
        Answer::Choice { ids } => {
            let labels = chosen(question, &ids);
            if labels.is_empty() {
                return Err(ToolError::Failed(format!(
                    "the answer to {:?} named none of the options",
                    question.question
                )));
            }
            Ok(format!("{}: {}", question.header, labels.join(", ")))
        }
        Answer::Text { text } => Ok(format!("{}: {text}", question.header)),
        other => Err(ToolError::Failed(format!(
            "unexpected answer to {:?}: {other:?}",
            question.question
        ))),
    }
}

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "AskUserQuestion".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<AskArgs>(),
            meta: Default::default(),
        }
    }

    /// It reads nothing and writes nothing, but it holds the turn on a person,
    /// so nothing else may run beside it.
    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits {
            read_only: true,
            concurrency_safe: false,
            trusted: true,
            ..ToolTraits::default()
        }
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: AskArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        validate(&args)?;

        let mut lines = Vec::new();
        for question in &args.questions {
            let kind = interaction(question);
            let specs = answer_specs(&kind);
            let answer = cx
                .call
                .ask(kind, specs)
                .await
                .map_err(|e| ToolError::Failed(format!("the question was not put: {e}")))?;
            if matches!(answer, Answer::Cancel | Answer::Deny { .. }) {
                return Ok(ToolOutput::error(format!(
                    "The user declined to answer: {}",
                    question.question
                )));
            }
            lines.push(answered(question, answer)?);
        }
        Ok(ToolOutput::text(format!(
            "The user answered:\n{}",
            lines.join("\n")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{ScriptedHost, context_with};
    use std::sync::Arc;

    fn one_question() -> Value {
        serde_json::json!({
            "questions": [{
                "question": "Which authentication method?",
                "header": "Auth method",
                "options": [
                    { "label": "OAuth", "description": "Redirect to the provider" },
                    { "label": "API key" }
                ]
            }]
        })
    }

    async fn ask(input: Value, answers: Vec<Answer>) -> (ToolOutput, Arc<ScriptedHost>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let host = ScriptedHost::new(answers);
        let cx = context_with(dir.path(), host.clone());
        let out = AskUserQuestionTool.call(input, &cx).await.expect("ask");
        (out, host)
    }

    #[test]
    fn the_spec_advertises_the_argument_schema() {
        let spec = AskUserQuestionTool.spec();
        assert_eq!(spec.name, "AskUserQuestion");
        assert!(spec.input_schema["properties"]["questions"]["description"].is_string());
        let traits = AskUserQuestionTool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && !traits.concurrency_safe);
        assert!(
            AskUserQuestionTool
                .subjects(&Value::Null, std::path::Path::new("/"))
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_question_becomes_an_interaction_and_its_answer_a_label() {
        let (out, host) = ask(
            one_question(),
            vec![Answer::Choice {
                ids: vec!["0".into()],
            }],
        )
        .await;
        assert_eq!(
            out.parts[0].as_text(),
            Some("The user answered:\nAuth method: OAuth")
        );
        assert!(!out.is_error);

        let asked = host.asked();
        assert_eq!(asked.len(), 1);
        let (kind, specs) = &asked[0];
        assert_eq!(
            specs,
            &vec![AnswerSpec::Choice, AnswerSpec::Text, AnswerSpec::Cancel]
        );
        let InteractionKind::Question {
            question,
            header,
            options,
            free_text,
            multi,
        } = kind
        else {
            panic!("expected a question, got {kind:?}");
        };
        assert_eq!(question, "Which authentication method?");
        assert_eq!(header.as_deref(), Some("Auth method"));
        assert!(*free_text && !*multi);
        assert_eq!(
            options,
            &vec![
                QuestionOption {
                    id: "0".into(),
                    label: "OAuth".into(),
                    description: Some("Redirect to the provider".into()),
                    role: None,
                },
                QuestionOption {
                    id: "1".into(),
                    label: "API key".into(),
                    description: None,
                    role: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn every_question_is_asked_in_turn_and_answered_by_its_header() {
        let input = serde_json::json!({
            "questions": [
                {
                    "question": "Which store?",
                    "header": "Store",
                    "options": [{ "label": "Postgres" }, { "label": "SQLite" }]
                },
                {
                    "question": "Which runtime?",
                    "header": "Runtime",
                    "options": [{ "label": "tokio" }, { "label": "smol" }]
                }
            ]
        });
        let (out, host) = ask(
            input,
            vec![
                Answer::Choice {
                    ids: vec!["1".into()],
                },
                Answer::Text {
                    text: "async-std".into(),
                },
            ],
        )
        .await;
        assert_eq!(
            out.parts[0].as_text(),
            Some("The user answered:\nStore: SQLite\nRuntime: async-std")
        );
        assert_eq!(host.asked().len(), 2);
    }

    #[tokio::test]
    async fn a_multi_select_question_reports_every_label_chosen() {
        let input = serde_json::json!({
            "questions": [{
                "question": "Which targets?",
                "header": "Targets",
                "options": [{ "label": "linux" }, { "label": "macos" }, { "label": "windows" }],
                "multi_select": true
            }]
        });
        let (out, host) = ask(
            input,
            vec![Answer::Choice {
                ids: vec!["0".into(), "2".into()],
            }],
        )
        .await;
        assert_eq!(
            out.parts[0].as_text(),
            Some("The user answered:\nTargets: linux, windows")
        );
        let (kind, _) = &host.asked()[0];
        assert!(matches!(
            kind,
            InteractionKind::Question { multi: true, .. }
        ));
    }

    #[tokio::test]
    async fn declining_is_an_error_result_that_says_so() {
        for answer in [Answer::Cancel, Answer::Deny { feedback: None }] {
            let (out, _) = ask(one_question(), vec![answer]).await;
            assert!(out.is_error);
            assert_eq!(
                out.parts[0].as_text(),
                Some("The user declined to answer: Which authentication method?")
            );
        }
    }

    #[tokio::test]
    async fn an_answer_naming_no_option_fails() {
        let dir = tempfile::tempdir().expect("temp dir");
        let host = ScriptedHost::new(vec![Answer::Choice {
            ids: vec!["7".into()],
        }]);
        let cx = context_with(dir.path(), host);
        let error = AskUserQuestionTool.call(one_question(), &cx).await.err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.contains("named none of the options")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_host_that_cannot_ask_fails_the_call() {
        let dir = tempfile::tempdir().expect("temp dir");
        let host = ScriptedHost::new(vec![]);
        let cx = context_with(dir.path(), host);
        let error = AskUserQuestionTool.call(one_question(), &cx).await.err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("the question was not put:")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_malformed_set_of_questions_is_refused_before_anyone_is_asked() {
        let option = serde_json::json!({ "label": "a" });
        let question = |q: &str, options: Value| serde_json::json!({ "question": q, "header": "H", "options": options });
        let cases = vec![
            (
                serde_json::json!({ "questions": [] }),
                "ask 1 to 4 questions",
            ),
            (
                serde_json::json!({ "questions": (0..5)
                    .map(|i| question(&format!("q{i}?"), serde_json::json!([option, { "label": "b" }])))
                    .collect::<Vec<_>>() }),
                "ask 1 to 4 questions",
            ),
            (
                serde_json::json!({ "questions": [question("q?", serde_json::json!([option]))] }),
                "needs 2 to 4 options",
            ),
            (
                serde_json::json!({ "questions": [
                    question("same?", serde_json::json!([option, { "label": "b" }])),
                    question("same?", serde_json::json!([option, { "label": "b" }])),
                ] }),
                "is asked twice",
            ),
            (
                serde_json::json!({ "questions": [question("q?", serde_json::json!([option, option]))] }),
                "is offered twice",
            ),
        ];
        for (input, expected) in cases {
            let dir = tempfile::tempdir().expect("temp dir");
            let host = ScriptedHost::new(vec![]);
            let cx = context_with(dir.path(), host.clone());
            let error = AskUserQuestionTool.call(input, &cx).await.err();
            assert!(
                matches!(&error, Some(ToolError::InvalidInput(m)) if m.contains(expected)),
                "expected {expected:?}, got {error:?}"
            );
            assert!(host.asked().is_empty(), "nobody should have been asked");
        }
    }
}
