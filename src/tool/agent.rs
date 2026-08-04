use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;

use crate::permission::PermissionMode;
use crate::query::{Session, UiHooks};
use crate::tool::{parse_input, Tool, ToolContext, ToolError, ToolResult};

const MAX_AGENT_DEPTH: usize = 3;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentInput {
    #[schemars(description = "子代理的独立任务指令")]
    prompt: String,
    /// 后台化：立即返回 async_launched，完成时通知主 agent。
    #[serde(default)]
    #[schemars(description = "后台执行（默认 false）：立即返回任务 id，完成时通知")]
    background: Option<bool>,
    /// 任务简述（可选），随 header 显示。
    #[serde(default)]
    #[allow(dead_code)]
    #[schemars(description = "任务简述（可选）")]
    description: Option<String>,
}

/// 子代理工具（对标 Claude Code Task，D14）：递归 queryLoop，
/// 独立消息历史，结果文本回填父模型。
pub struct AgentTool {
    session: Arc<Session>,
}

impl AgentTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }
}

/// 子代理 UI：捕获文本、无交互（写工具在非 bypass 模式下被拒）。
/// cell 记录已产出字符数（后台 agent 的 interval 进度检查）。
fn subagent_hooks(
    output: Arc<Mutex<String>>,
    cell: Arc<AgentCell>,
    permission_mode: PermissionMode,
) -> UiHooks {
    let bypass = permission_mode == PermissionMode::BypassPermissions;
    UiHooks {
        on_event: Box::new(move |event| {
            if let crate::api::types::StreamEvent::TextDelta { text, .. } = event
                && let Ok(mut output) = output.lock()
            {
                output.push_str(text);
                cell.record_chars(text.chars().count());
            }
        }),
        on_tool_ready: Box::new(|_name, _input| {}),
        on_tool_done: Box::new(|_| {}),
        on_round_end: Box::new(|| {}),
        on_warning: Box::new(|_| {}),
        ask: Box::new(move |_tool_name, _reason| Box::pin(async move { bypass })),
    }
}

/// 后台化子 agent：注册 watchable（interval 检查产出量）+ spawn 执行，
/// 立即返回 async_launched；完成/失败经 registry 通知。
impl AgentTool {
    fn launch_background(
        &self,
        params: &AgentInput,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let cell = Arc::new(AgentCell::new());
        let label = format!(
            "Agent: {}",
            params
                .description
                .clone()
                .unwrap_or_else(|| params.prompt.chars().take(40).collect())
        );
        let id = ctx.watch.register(Box::new(AgentWatch {
            cell: cell.clone(),
            label: label.clone(),
            interval: Some(std::time::Duration::from_secs(5)),
        }));
        let sub_session = self.build_sub_session();
        let prompt = params.prompt.clone();
        let watch = ctx.watch.clone();
        let permission_mode = sub_session.permission_mode;
        tokio::spawn(async move {
            let output = Arc::new(Mutex::new(String::new()));
            let mut ui = subagent_hooks(output.clone(), cell, permission_mode);
            match crate::query::run_query(&sub_session, Vec::new(), &prompt, &mut ui, None).await {
                Ok(_messages) => {
                    let text = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    watch.set_state(
                        id,
                        crate::watch::WatchState::Done,
                        Some("完成".to_string()),
                        Some(serde_json::json!(if text.trim().is_empty() {
                            "[subagent returned no text]"
                        } else {
                            text.as_str()
                        })),
                    );
                }
                Err(e) => {
                    watch.set_state(
                        id,
                        crate::watch::WatchState::Failed,
                        Some(format!("subagent failed: {e}")),
                        None,
                    );
                }
            }
        });
        Ok(ToolResult {
            content: serde_json::Value::String(serde_json::json!({
                "status": "async_launched",
                "task_id": id.0,
                "label": label,
                "note": "子代理已在后台执行，完成通知会注入下一轮上下文",
            })
            .to_string()),
            is_error: false,
            diff: None,
        })
    }

    fn build_sub_session(&self) -> Arc<Session> {
        Arc::new(Session {
            client: self.session.client.clone(),
            model: self.session.model.clone(),
            permission_mode: self.session.permission_mode,
            settings: self.session.settings.clone(),
            system: self.session.system.clone(),
            transcript: None,
            depth: self.session.depth + 1,
            home: self.session.home.clone(),
            quiet: self.session.quiet,
            compact_failures: self.session.compact_failures.clone(),
            watch: self.session.watch.clone(),
        })
    }
}

/// 后台 agent 进度：已产出字符数（interval poll 用）。
struct AgentCell {
    chars: std::sync::atomic::AtomicUsize,
}

impl AgentCell {
    fn new() -> Self {
        Self {
            chars: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    fn record_chars(&self, n: usize) {
        self.chars
            .fetch_add(n, std::sync::atomic::Ordering::SeqCst);
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        crate::watch::WatchPoll {
            state: crate::watch::WatchState::Running,
            detail: Some(format!(
                "已产出 {} 字符",
                self.chars.load(std::sync::atomic::Ordering::SeqCst)
            )),
            payload: None,
        }
    }
}

struct AgentWatch {
    cell: Arc<AgentCell>,
    label: String,
    interval: Option<std::time::Duration>,
}

impl crate::watch::Watchable for AgentWatch {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        self.cell.poll()
    }
    fn check_interval(&self) -> Option<std::time::Duration> {
        self.interval
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> String {
        "Agent".to_string()
    }

    fn description(&self) -> String {
        "派生子代理执行独立任务（深度受限），返回其最终结论。".to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<AgentInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: AgentInput = parse_input(&input)?;
        if self.session.depth >= MAX_AGENT_DEPTH {
            return Err(ToolError::failed(format!(
                "max agent depth ({MAX_AGENT_DEPTH}) exceeded"
            )));
        }
        if params.background.unwrap_or(false) {
            return self.launch_background(&params, ctx);
        }

        let sub_session = self.build_sub_session();

        let output = Arc::new(Mutex::new(String::new()));
        let mut ui = subagent_hooks(
            output.clone(),
            Arc::new(AgentCell::new()),
            sub_session.permission_mode,
        );
        match crate::query::run_query(&sub_session, Vec::new(), &params.prompt, &mut ui, None)
            .await
        {
            Ok(_messages) => {
                let text = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
                Ok(ToolResult {
                    content: serde_json::Value::String(if text.trim().is_empty() {
                        "[subagent returned no text]".to_string()
                    } else {
                        text
                    }),
                    is_error: false,
                    diff: None,
                })
            }
            Err(e) => Err(ToolError::failed(format!("subagent failed: {e}"))),
        }
    }
}
