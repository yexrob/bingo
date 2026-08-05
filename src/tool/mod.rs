use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;

use crate::settings::HooksConfig;
use crate::tasks::TaskStore;

pub mod agent;
pub mod bash;
pub mod diff;
pub mod edit;
pub mod executor;
pub mod glob;
pub mod grep;
pub mod read;
pub mod task;
pub mod webfetch;
pub mod websearch;
pub mod write;

/// 工具执行上下文：随 queryLoop 一轮共享。
#[derive(Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    /// Watchable 注册中心（后台任务生命周期与通知）。
    pub watch: std::sync::Arc<crate::watch::WatchRegistry>,
    /// 共享 HTTP 客户端（WebFetch/WebSearch 复用连接池；不跟随重定向）。
    pub http: reqwest::Client,
    /// Task 存储（Task 工具族；TUI 任务区同源）。
    pub tasks: std::sync::Arc<TaskStore>,
    /// Hooks 配置（TaskCreated/TaskCompleted 事件）。
    pub hooks: HooksConfig,
    /// 权限模式字符串（hook 输入契约）。
    pub permission_mode: String,
    /// 任务区展开信号（对标 CC set_expanded_view: tasks；headless 无订阅者）。
    pub expand_tasks: tokio::sync::watch::Sender<bool>,
}

impl ToolContext {
    /// 工具调用时通知 TUI 展开任务区（对标 Claude Code set_expanded_view: tasks）。
    pub fn set_expanded_view_tasks(&self) {
        let _ = self.expand_tasks.send(true);
    }
}

/// 工具执行结果：content 即回填给模型的 tool_result content。
/// diff 为可选 unified diff 文本（Edit/Write 等编辑工具的 UI 预览，不回填模型）。
#[derive(Debug, Default)]
pub struct ToolResult {
    pub content: serde_json::Value,
    pub is_error: bool,
    pub diff: Option<String>,
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("{0}")]
    Failed(String),
}

impl ToolError {
    pub fn failed(msg: impl Into<String>) -> Self {
        Self::Failed(msg.into())
    }
}

/// Tool 契约（对标 Claude Code Tool.ts，D2）。
/// 默认 fail-closed：非并发安全、非只读、允许（权限交给统一门）。
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn input_schema(&self) -> serde_json::Value;
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_destructive(&self, _input: &serde_json::Value) -> bool {
        false
    }
    /// 编辑类工具（Edit/Write 等）：acceptEdits 模式下自动允许，其他模式照常询问。
    fn is_edit_tool(&self, _input: &serde_json::Value) -> bool {
        false
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError>;
}

/// 模型回传参数 → 目标类型。失败信息给模型可见（is_error 回填）。
pub fn parse_input<T: for<'a> Deserialize<'a>>(
    input: &serde_json::Value,
) -> Result<T, ToolError> {
    serde_json::from_value(input.clone()).map_err(|e| ToolError::failed(format!("bad input: {e}")))
}

/// 由 input 结构体生成 inputSchema（schemars，单一来源，D2）。
/// 去掉 `$schema` 键：工具 schema 随 tool_params 发给模型，保持既有形状。
pub fn schema_for<T: schemars::JsonSchema>() -> serde_json::Value {
    let mut generator = schemars::r#gen::SchemaGenerator::default();
    serde_json::to_value(T::json_schema(&mut generator))
        .unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
}

/// 注册表里按名字找工具。
pub fn find_tool<'a>(tools: &'a [Box<dyn Tool>], name: &str) -> Option<&'a dyn Tool> {
    tools.iter().map(|t| t.as_ref()).find(|t| t.name() == name)
}

/// 组装发送给 API 的 tools 参数。
pub fn tool_params(tools: &[Box<dyn Tool>]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name(),
                "description": t.description(),
                "input_schema": t.input_schema(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::tool::bash::BashTool;
    use crate::tool::read::ReadTool;

    #[test]
    fn agent_schema_matches_input_struct() {
        // 回归：schema 与 input 结构体单一来源（D2）。曾漂移——结构体缺 description。
        let schema = schema_for::<agent::AgentInput>();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["prompt"]));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert!(schema["properties"]["description"].is_object());
        assert_eq!(schema["properties"]["description"]["type"], json!(["string", "null"]));
    }

    #[test]
    fn read_schema_shape() {
        let schema = ReadTool::new().input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["file_path"]));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["properties"]["file_path"]["type"], "string");
        assert_eq!(schema["properties"]["file_path"]["description"], "要读取的文件路径（绝对或相对）");
    }

    #[test]
    fn bash_schema_optional_timeout() {
        let schema = BashTool::new().input_schema();
        assert_eq!(schema["required"], json!(["command"]));
        // Option 字段不入 required
        assert!(!schema["required"].as_array().unwrap().contains(&json!("timeout")));
    }

    #[test]
    fn schema_has_no_dollar_schema_key() {
        let schema = ReadTool::new().input_schema();
        assert!(schema.get("$schema").is_none(), "发给模型的形状不含 $schema: {schema}");
    }
}
