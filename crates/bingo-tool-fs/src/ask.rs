//! `AskUserQuestion`: the model's questions, one interaction for all of them.
//! The kernel opens and resolves it; the tool only asks and reports what came
//! back — one line per question, in the order they were asked (M53).

use std::collections::HashSet;

use async_trait::async_trait;
use bingo_sdk::{
    Answer, AnswerSpec, InteractionKind, Question, QuestionOption, Tool, ToolContext, ToolError,
    ToolOutput, ToolSpec, ToolTraits, input_schema,
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
with two to four options; put the option you recommend first. All of them are \
shown together, one tab per question, and answered in one go, so ask \
everything you need at once rather than in a row of separate calls. Do not \
offer an \"Other\" option — the user can always type an answer of their own, \
or decline a question. An option may carry a preview: a few lines of \
monospace shown beside it while the user is on it, for a choice they want to \
see rather than read — a layout, a snippet, two configurations to compare. \
The result gives back what the user chose, one line per question; a question \
they left alone reads `skipped`.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskArgs {
    /// The questions to ask, one to four, all shown together and answered in
    /// one go.
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
    /// A few lines of monospace shown beside the option while the user is on
    /// it: the layout, snippet or configuration picking it would mean. On
    /// single-select questions only.
    pub preview: Option<String>,
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
        // A preview belongs to the option the cursor is on, and a set has no
        // one option under the cursor to show one for.
        if option.preview.is_some() && question.multi_select {
            return Err(ToolError::InvalidInput(format!(
                "the option {:?} carries a preview, which a multi-select question cannot show",
                option.label
            )));
        }
    }
    Ok(())
}

/// The one interaction the kernel opens for the whole set (M53).
fn interaction(args: &AskArgs) -> InteractionKind {
    InteractionKind::Form {
        // The model's questions say what they are about themselves.
        title: None,
        questions: args.questions.iter().map(asked).collect(),
    }
}

/// One question as the kernel puts it. The option's position is its id, so an
/// answer maps back to the label the model wrote.
fn asked(question: &AskQuestion) -> Question {
    Question {
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
                preview: option.preview.clone(),
            })
            .collect(),
        // A person may always answer in their own words, which is why the
        // model is told not to offer an "Other" option.
        free_text: true,
        multi: question.multi_select,
    }
}

/// The answers the kernel will accept: the whole form, or nothing.
fn answer_specs() -> Vec<AnswerSpec> {
    vec![AnswerSpec::Form, AnswerSpec::Cancel]
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

/// One answered question, as the model reads it back. A question the person
/// left alone is `skipped` rather than an absence.
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
        Answer::Cancel | Answer::Deny { .. } => Ok(format!("{}: skipped", question.header)),
        other => Err(ToolError::Failed(format!(
            "unexpected answer to {:?}: {other:?}",
            question.question
        ))),
    }
}

/// The form's answers against the questions they belong to. The kernel gives
/// back one answer per question in order; anything else is a broken door, not
/// a person's choice.
fn lines(args: &AskArgs, answers: Vec<Answer>) -> Result<Vec<String>, ToolError> {
    if answers.len() != args.questions.len() {
        return Err(ToolError::Failed(format!(
            "the form came back with {} answers for {} questions",
            answers.len(),
            args.questions.len()
        )));
    }
    args.questions
        .iter()
        .zip(answers)
        .map(|(question, answer)| answered(question, answer))
        .collect()
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

        let answer = cx
            .call
            .ask(interaction(&args), answer_specs())
            .await
            .map_err(|e| ToolError::Failed(format!("the questions were not put: {e}")))?;
        let Answer::Form { answers } = answer else {
            return Ok(ToolOutput::error(declined(&args)));
        };
        Ok(ToolOutput::text(format!(
            "The user answered:\n{}",
            lines(&args, answers)?.join("\n")
        )))
    }
}

/// Leaving the whole card is declining all of it, in the words of the first
/// question — the one a person was looking at when they left.
fn declined(args: &AskArgs) -> String {
    match args.questions.first() {
        Some(first) => format!("The user declined to answer: {}", first.question),
        None => "The user declined to answer.".to_string(),
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
                    { "label": "OAuth", "description": "Redirect to the provider",
                  "preview": "GET /authorize" },
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

    /// One answer per question, wrapped as the form the card sends back.
    fn form(answers: Vec<Answer>) -> Vec<Answer> {
        vec![Answer::Form { answers }]
    }

    #[tokio::test]
    async fn a_question_becomes_an_interaction_and_its_answer_a_label() {
        let (out, host) = ask(
            one_question(),
            form(vec![Answer::Choice {
                ids: vec!["0".into()],
            }]),
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
        assert_eq!(specs, &vec![AnswerSpec::Form, AnswerSpec::Cancel]);
        let InteractionKind::Form { questions, .. } = kind else {
            panic!("expected a form, got {kind:?}");
        };
        let [
            Question {
                question,
                header,
                options,
                free_text,
                multi,
            },
        ] = &questions[..]
        else {
            panic!("expected one question, got {questions:?}");
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
                    preview: Some("GET /authorize".into()),
                },
                QuestionOption {
                    id: "1".into(),
                    label: "API key".into(),
                    description: None,
                    role: None,
                    preview: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn every_question_is_asked_at_once_and_answered_by_its_header() {
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
            form(vec![
                Answer::Choice {
                    ids: vec!["1".into()],
                },
                Answer::Text {
                    text: "async-std".into(),
                },
            ]),
        )
        .await;
        assert_eq!(
            out.parts[0].as_text(),
            Some("The user answered:\nStore: SQLite\nRuntime: async-std")
        );
        let asked = host.asked();
        assert_eq!(asked.len(), 1, "one interaction for the whole set");
        let InteractionKind::Form { questions, .. } = &asked[0].0 else {
            panic!("expected a form, got {:?}", asked[0].0);
        };
        assert_eq!(questions.len(), 2);
    }

    /// A question the person walked past is `skipped`, and the ones they did
    /// answer still reach the model.
    #[tokio::test]
    async fn a_question_left_alone_reads_as_skipped() {
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
        let (out, _) = ask(
            input,
            form(vec![
                Answer::Choice {
                    ids: vec!["0".into()],
                },
                Answer::Cancel,
            ]),
        )
        .await;
        assert_eq!(
            out.parts[0].as_text(),
            Some("The user answered:\nStore: Postgres\nRuntime: skipped")
        );
        assert!(!out.is_error);
    }

    /// A door that gives back the wrong number of answers is a broken door,
    /// not a person's choice.
    #[tokio::test]
    async fn a_form_answered_with_the_wrong_count_fails() {
        let dir = tempfile::tempdir().expect("temp dir");
        let host = ScriptedHost::new(form(vec![Answer::Cancel, Answer::Cancel]));
        let cx = context_with(dir.path(), host);
        let error = AskUserQuestionTool.call(one_question(), &cx).await.err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.contains("2 answers for 1 questions")),
            "got {error:?}"
        );
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
            form(vec![Answer::Choice {
                ids: vec!["0".into(), "2".into()],
            }]),
        )
        .await;
        assert_eq!(
            out.parts[0].as_text(),
            Some("The user answered:\nTargets: linux, windows")
        );
        let (kind, _) = &host.asked()[0];
        let InteractionKind::Form { questions, .. } = kind else {
            panic!("expected a form, got {kind:?}");
        };
        assert!(questions[0].multi);
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
        let host = ScriptedHost::new(form(vec![Answer::Choice {
            ids: vec!["7".into()],
        }]));
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
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("the questions were not put:")),
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
            (
                serde_json::json!({ "questions": [{
                    "question": "q?",
                    "header": "H",
                    "options": [{ "label": "a", "preview": "one\ntwo" }, { "label": "b" }],
                    "multi_select": true,
                }] }),
                "which a multi-select question cannot show",
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
