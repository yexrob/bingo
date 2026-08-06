use std::collections::HashSet;

use async_trait::async_trait;
use serde::Deserialize;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// 单条问题：
/// options 2-4 个、label 唯一；header 为短标签（≤12 字符）。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct AskQuestion {
    #[schemars(description = "要问用户的完整问题，应清晰具体并以问号结尾")]
    question: String,
    #[schemars(description = "短标签（≤12 字符），如「认证方式」「技术选型」")]
    header: Option<String>,
    #[schemars(description = "可选答案（2-4 个），label 需互不相同", length(min = 2, max = 4))]
    options: Vec<AskOption>,
    #[serde(rename = "multiSelect", default)]
    #[schemars(rename = "multiSelect", description = "多选（暂不支持，传 true 会报错）")]
    multi_select: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct AskOption {
    #[schemars(description = "选项文本（1-5 词，清楚描述该选择；不要提供 \"Other\" 选项，会自动添加）")]
    label: String,
    #[schemars(description = "选项说明：该选项的含义或选择后的结果（可选）")]
    description: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct AskUserQuestionInput {
    #[schemars(description = "要问的问题（1-4 个；逐个询问）", length(min = 1, max = 4))]
    questions: Vec<AskQuestion>,
}

const MAX_QUESTIONS: usize = 4;
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 4;

/// 问用户选择题：暂停回合，用户以数字键回答，
/// Esc 跳过。答案回填模型（`The user answered: "q"="a"`）；全部未答 →
/// `The user did not answer the questions.`
pub struct AskUserQuestionTool;

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn name(&self) -> String {
        "AskUserQuestion".to_string()
    }

    fn description(&self) -> String {
        "向用户提出选择题以收集信息、澄清歧义、了解偏好或提供选项供决策。\
         仅当答案会改变你接下来的做法时才使用——有常规默认值或能从代码库自行核实的事实不要问，\
         直接选明显的那项并在回复中说明即可。推荐项放第一个并在 label 末尾加 \"(Recommended)\"。\
         问题 1-4 个、每题选项 2-4 个（模型请求超限会被拒绝，自行拆分）。\
         不要提供 \"Other\" 选项，用户总是可以选 Other 自定义输入文本。\
         用户总是可以用 Esc 跳过问题（返回未回答），不要为此反复重问。"
            .to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<AskUserQuestionInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        // 阻塞等待用户回答，不能与其他工具并行。
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

/// 输入约束：questions 1-4、options 2-4、问题与选项各自唯一。
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
            // 空 questions
            serde_json::json!({"questions": []}),
            // 5 个问题
            serde_json::json!({"questions": (0..5).map(|i| serde_json::json!({
                "question": format!("q{i}?"),
                "options": [{"label": "a"}, {"label": "b"}],
            })).collect::<Vec<_>>()}),
            // 单选项
            serde_json::json!({"questions": [{
                "question": "q?",
                "options": [{"label": "a"}],
            }]}),
            // 重复 label
            serde_json::json!({"questions": [{
                "question": "q?",
                "options": [{"label": "a"}, {"label": "a"}],
            }]}),
            // 重复问题文本
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

    /// 端到端：AskUserQuestion 经执行队列调用，ask_question 回调返回选项索引，
    /// 结果按 CC 格式回填。
    #[tokio::test]
    async fn asks_and_returns_answer() {
        let ctx = ToolContext {
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

    /// Other 自由输入：答案回填为自定义文本（CC 自动提供的 Other 选项）。
    #[tokio::test]
    async fn other_text_answer_backfills_raw_text() {
        let ctx = ToolContext {
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

    /// Esc 跳过（None）→ 未回答。
    #[tokio::test]
    async fn skipped_returns_did_not_answer() {
        let ctx = ToolContext {
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

    /// multiSelect 暂不支持：报错让模型改单选。
    #[tokio::test]
    async fn multi_select_rejected() {
        let ctx = ToolContext {
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

    /// 与 Bash 并置时执行队列串行化（AskUserQuestion 阻塞等待回答）。
    #[tokio::test]
    async fn ask_question_is_not_concurrency_safe() {
        let ctx = ToolContext {
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
