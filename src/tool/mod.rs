use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;

pub mod agent;
pub mod bash;
pub mod edit;
pub mod executor;
pub mod glob;
pub mod grep;
pub mod read;
pub mod webfetch;
pub mod write;

/// 工具执行上下文：随 queryLoop 一轮共享。
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
}

/// 工具执行结果：content 即回填给模型的 tool_result content。
#[derive(Debug)]
pub struct ToolResult {
    pub content: serde_json::Value,
    pub is_error: bool,
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
