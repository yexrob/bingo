use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;

use crate::permission::PermissionMode;
use crate::query::{Session, UiHooks};
use crate::tool::{parse_input, Tool, ToolContext, ToolError, ToolResult};

const MAX_AGENT_DEPTH: usize = 3;

#[derive(Debug, Deserialize)]
struct AgentInput {
    prompt: String,
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
fn subagent_hooks(output: Arc<Mutex<String>>, permission_mode: PermissionMode) -> UiHooks {
    let bypass = permission_mode == PermissionMode::BypassPermissions;
    UiHooks {
        on_event: Box::new(move |event| {
            if let crate::api::types::StreamEvent::TextDelta { text, .. } = event
                && let Ok(mut output) = output.lock()
            {
                output.push_str(text);
            }
        }),
        on_tool_done: Box::new(|_| {}),
        on_warning: Box::new(|_| {}),
        ask: Box::new(move |_tool_name, _reason| Box::pin(async move { bypass })),
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
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "子代理的独立任务指令"
                },
                "description": {
                    "type": "string",
                    "description": "任务简述（可选）"
                }
            },
            "required": ["prompt"],
            "additionalProperties": false
        })
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
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: AgentInput = parse_input(&input)?;
        if self.session.depth >= MAX_AGENT_DEPTH {
            return Err(ToolError::failed(format!(
                "max agent depth ({MAX_AGENT_DEPTH}) exceeded"
            )));
        }

        let sub_session = Arc::new(Session {
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
        });

        let output = Arc::new(Mutex::new(String::new()));
        let mut ui = subagent_hooks(output.clone(), sub_session.permission_mode);
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
