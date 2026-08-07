use std::collections::HashSet;

use async_trait::async_trait;
use serde::Deserialize;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// A single question:
/// 2-4 options with unique labels; header is a short tag (≤12 chars).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct AskQuestion {
    #[schemars(description = "The full question for the user; be clear and specific, end with a question mark")]
    question: String,
    #[schemars(description = "Short label (≤12 chars), e.g. \"Auth method\", \"Tech stack\"")]
    header: Option<String>,
    #[schemars(description = "Possible answers (2-4); labels must be unique", length(min = 2, max = 4))]
    options: Vec<AskOption>,
    #[serde(rename = "multiSelect", default)]
    #[schemars(rename = "multiSelect", description = "多选（暂不支持，传 true 会报错）")]
    multi_select: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct AskOption {
    #[schemars(description = "Option text (1-5 words, clearly describing the choice; do not provide an \"Other\" option — it is added automatically)")]
    label: String,
    #[schemars(description = "Option description: what this option means or what choosing it leads to (optional)")]
    description: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct AskUserQuestionInput {
    #[schemars(description = "Questions to ask (1-4; asked one by one)", length(min = 1, max = 4))]
    questions: Vec<AskQuestion>,
}

const MAX_QUESTIONS: usize = 4;
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 4;

/// Asks the user multiple-choice questions: the turn pauses and the user answers with number keys;
/// Esc skips. Answers are fed back to the model (`The user answered: "q"="a"`); if none were answered →
/// `The user did not answer the questions.`
pub struct AskUserQuestionTool;

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn name(&self) -> String {
        "AskUserQuestion".to_string()
    }

    fn description(&self) -> String {
        "Ask the user multiple-choice questions to gather information, clarify ambiguity, learn preferences, or offer options for a decision.\
         Use it only when the answer would change what you do next — don't ask when there is a sensible default or a fact you can verify from the codebase yourself;\
         pick the obvious option and state it in your reply. Put the recommended option first and append \"(Recommended)\" to its label.\
         1-4 questions, 2-4 options each (the model request is rejected over the limit; split them yourself).\
         Do not provide an \"Other\" option — the user can always type custom input via Other.\
         The user can always skip a question with Esc (it returns unanswered); don't re-ask repeatedly for it."
            .to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<AskUserQuestionInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        // Blocks waiting for the user's answer, so it cannot run in parallel with other tools.
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: AskUserQuestionInput = parse_input(&input)?;
        validate(&params)?;

        let mut answers: Vec<String> = Vec::new();
        for (i, q) in params.questions.iter().enumerate() {
            if q.multi_select.unwrap_or(false) {
                return Err(ToolError::failed(
                    "multiSelect 暂不支持：请拆成单选问题（multiSelect: false 或缺省）",
                ));
            }
            let labels: Vec<(String, Option<String>)> = q
                .options
                .iter()
                .map(|o| (o.label.clone(), o.description.clone()))
                .collect();
            let title = q
                .header
                .clone()
                .unwrap_or_else(|| format!("问题 {}", i + 1));
            match (ctx.ask_question)(title, q.question.clone(), labels).await {
                Some(crate::query::AskAnswer::Option(idx)) => {
                    answers.push(format!(
                        "\"{}\"=\"{}\"",
                        q.question,
                        q.options[idx].label
                    ));
                }
                Some(crate::query::AskAnswer::Other(text)) => {
                    answers.push(format!("\"{}\"=\"{}\"", q.question, text));
                }
                None => break,
            }
        }
        let text = if answers.is_empty() {
            "The user did not answer the questions.".to_string()
        } else {
            format!("The user answered: {}", answers.join(", "))
        };
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

/// Input constraints: 1-4 questions, 2-4 options each, and unique question/label texts.
fn validate(params: &AskUserQuestionInput) -> Result<(), ToolError> {
    let n = params.questions.len();
    if !(1..=MAX_QUESTIONS).contains(&n) {
        return Err(ToolError::failed(format!(
            "questions 数量须为 1-{MAX_QUESTIONS}（收到 {n}），请拆分后重试"
        )));
    }
    let mut seen = HashSet::new();
    for q in &params.questions {
        if !seen.insert(q.question.trim()) {
            return Err(ToolError::failed(format!(
                "问题文本必须唯一：{:?} 重复",
                q.question
            )));
        }
        let m = q.options.len();
        if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&m) {
            return Err(ToolError::failed(format!(
                "options 数量须为 {MIN_OPTIONS}-{MAX_OPTIONS}（问题 {:?} 收到 {m}）",
                q.question
            )));
        }
        let mut labels = HashSet::new();
        for o in &q.options {
            if !labels.insert(o.label.trim()) {
                return Err(ToolError::failed(format!(
                    "选项 label 必须唯一：{:?} 重复",
                    o.label
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::bash::BashTool;
    use crate::tool::executor::{execute_calls, PendingCall};

    #[test]
    fn schema_shape() {
        let schema = AskUserQuestionTool.input_schema();
        assert_eq!(schema["type"], "object");
        let q = &schema["properties"]["questions"];
        assert_eq!(q["type"], "array");
        assert_eq!(q["minItems"], serde_json::json!(1));
        assert_eq!(q["maxItems"], serde_json::json!(4));
        let opts = &schema["definitions"]["AskQuestion"]["properties"]["options"];
        assert_eq!(opts["minItems"], serde_json::json!(2));
        assert_eq!(opts["maxItems"], serde_json::json!(4));
    }

    #[test]
    fn rejects_bad_input() {
        let cases = [
            // empty questions
            serde_json::json!({"questions": []}),
            // 5 questions
            serde_json::json!({"questions": (0..5).map(|i| serde_json::json!({
                "question": format!("q{i}?"),
                "options": [{"label": "a"}, {"label": "b"}],
            })).collect::<Vec<_>>()}),
            // single option
            serde_json::json!({"questions": [{
                "question": "q?",
                "options": [{"label": "a"}],
            }]}),
            // duplicate label
            serde_json::json!({"questions": [{
                "question": "q?",
                "options": [{"label": "a"}, {"label": "a"}],
            }]}),
            // duplicate question text
            serde_json::json!({"questions": [
                {"question": "same?", "options": [{"label": "a"}, {"label": "b"}]},
                {"question": "same?", "options": [{"label": "c"}, {"label": "d"}]},
            ]}),
        ];
        for input in cases {
            let params: AskUserQuestionInput = parse_input(&input).unwrap();
            assert!(validate(&params).is_err(), "应拒绝: {input}");
        }
    }

    /// End-to-end: AskUserQuestion is invoked through the execution queue; the ask_question callback
    /// returns an option index and the result is fed back in the CC format.
    #[tokio::test]
    async fn asks_and_returns_answer() {
        let ctx = ToolContext {
            home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                "test",
            )),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|title, question, options| {
                assert_eq!(title, "技术选型");
                assert_eq!(question, "用哪个库？");
                assert_eq!(
                    options,
                    vec![
                        ("A (Recommended)".to_string(), None),
                        ("B".to_string(), Some("更快".to_string())),
                    ]
                );
                Box::pin(async { Some(crate::query::AskAnswer::Option(1)) })
            }),
        };
        let tool = AskUserQuestionTool;
        let result = tool
            .call(
                serde_json::json!({"questions": [{
                    "question": "用哪个库？",
                    "header": "技术选型",
                    "options": [
                        {"label": "A (Recommended)"},
                        {"label": "B", "description": "更快"},
                    ],
                }]}),
                &ctx,
            )
            .await
            .unwrap();
        let text = result.content.as_str().unwrap();
        assert_eq!(text, "The user answered: \"用哪个库？\"=\"B\"");
    }

    /// Other free-form input: the answer is fed back as custom text (the Other option CC provides automatically).
    #[tokio::test]
    async fn other_text_answer_backfills_raw_text() {
        let ctx = ToolContext {
            home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                "test",
            )),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| {
                Box::pin(async { Some(crate::query::AskAnswer::Other("用 serde".to_string())) })
            }),
        };
        let tool = AskUserQuestionTool;
        let result = tool
            .call(
                serde_json::json!({"questions": [{
                    "question": "用哪个库？",
                    "options": [{"label": "A"}, {"label": "B"}],
                }]}),
                &ctx,
            )
            .await
            .unwrap();
        let text = result.content.as_str().unwrap();
        assert_eq!(text, "The user answered: \"用哪个库？\"=\"用 serde\"");
    }

    /// Esc skip (None) → treated as not answered.
    #[tokio::test]
    async fn skipped_returns_did_not_answer() {
        let ctx = ToolContext {
            home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                "test",
            )),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        };
        let tool = AskUserQuestionTool;
        let result = tool
            .call(
                serde_json::json!({"questions": [{
                    "question": "q?",
                    "options": [{"label": "a"}, {"label": "b"}],
                }]}),
                &ctx,
            )
            .await
            .unwrap();
        let text = result.content.as_str().unwrap();
        assert_eq!(text, "The user did not answer the questions.");
    }

    /// multiSelect is not supported yet: the error instructs the model to use single-select.
    #[tokio::test]
    async fn multi_select_rejected() {
        let ctx = ToolContext {
            home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                "test",
            )),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        };
        let tool = AskUserQuestionTool;
        let err = tool
            .call(
                serde_json::json!({"questions": [{
                    "question": "q?",
                    "options": [{"label": "a"}, {"label": "b"}],
                    "multiSelect": true,
                }]}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("multiSelect"), "{err}");
    }

    /// When placed alongside Bash, the execution queue serializes them (AskUserQuestion blocks for the answer).
    #[tokio::test]
    async fn ask_question_is_not_concurrency_safe() {
        let ctx = ToolContext {
            home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                "test",
            )),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        };
        let ask = AskUserQuestionTool;
        let bash = BashTool::new();
        let calls: Vec<PendingCall> = vec![
            PendingCall {
                tool_use_id: "ask".into(),
                tool: &ask,
                input: serde_json::json!({"questions": [{
                    "question": "q?",
                    "options": [{"label": "a"}, {"label": "b"}],
                }]}),
            },
            PendingCall {
                tool_use_id: "bash".into(),
                tool: &bash,
                input: serde_json::json!({"command": "echo hi"}),
            },
        ];
        let (outcomes, _interrupted) = execute_calls(calls, &ctx, None).await;
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes[0].result.is_ok(), "ask 串行完成");
        assert!(outcomes[1].result.is_ok(), "bash 在 ask 之后执行");
    }
}
