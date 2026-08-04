use std::collections::HashMap;
use std::sync::Arc;

use rmcp::model::{CallToolRequestParams, Tool as McpToolModel};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use async_trait::async_trait;
use rmcp::{RoleClient, serve_client};
use thiserror::Error;
use tokio::process::Command as TokioCommand;

use crate::settings::McpServerConfig;
use crate::tool::{Tool, ToolContext, ToolError, ToolResult};

#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP server {server}: {detail}")]
    Connect { server: String, detail: String },
}

type Service = RunningService<RoleClient, ()>;

/// 连接全部 mcpServers，把每个工具适配成 Tool（isMcp）。
pub async fn connect_servers(
    servers: &HashMap<String, McpServerConfig>,
) -> Result<Vec<Box<dyn Tool>>, McpError> {
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    for (server_name, config) in servers {
        let mut command = TokioCommand::new(&config.command);
        command.args(&config.args);
        command.envs(&config.env);
        let transport = TokioChildProcess::new(command).map_err(|e| McpError::Connect {
            server: server_name.clone(),
            detail: e.to_string(),
        })?;
        let service = serve_client((), transport).await.map_err(|e| McpError::Connect {
            server: server_name.clone(),
            detail: e.to_string(),
        })?;
        let listed = service
            .list_all_tools()
            .await
            .map_err(|e| McpError::Connect {
                server: server_name.clone(),
                detail: e.to_string(),
            })?;
        let tool_count = listed.len();
        let service = Arc::new(service);
        for tool in listed {
            tools.push(Box::new(McpTool::new(server_name, tool, service.clone())));
        }
        eprintln!("[bingo] connected MCP server {server_name}: {tool_count} tools");
    }
    Ok(tools)
}

/// MCP 工具适配器：与内置工具共用同一 Tool trait。
pub struct McpTool {
    /// 模型可见名：mcp__{server}__{tool}
    name: String,
    description: String,
    input_schema: serde_json::Value,
    tool_name: String,
    service: Arc<Service>,
}

impl McpTool {
    fn new(server_name: &str, tool: McpToolModel, service: Arc<Service>) -> Self {
        let tool_name = tool.name.to_string();
        Self {
            name: format!("mcp__{server_name}__{tool_name}"),
            description: tool.description.map(|d| d.to_string()).unwrap_or_default(),
            input_schema: serde_json::to_value(&*tool.input_schema).unwrap_or_default(),
            tool_name,
            service,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn input_schema(&self) -> serde_json::Value {
        self.input_schema.clone()
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
        let arguments = input
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<serde_json::Map<_, _>>();
        let mut params = CallToolRequestParams::new(self.tool_name.clone());
        params.arguments = Some(arguments);
        let result = self
            .service
            .call_tool(params)
            .await
            .map_err(|e| ToolError::failed(format!("mcp call failed: {e}")))?;

        let mut text = String::new();
        for block in &result.content {
            if let Some(t) = block.as_text() {
                text.push_str(&t.text);
            } else if let Some(image) = block.as_image() {
                text.push_str(&format!("[image: {} bytes]", image.data.len()));
            } else {
                text.push_str(&format!("[{:?}]", block));
            }
            text.push('\n');
        }
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: result.is_error.unwrap_or(false),
        })
    }
}
