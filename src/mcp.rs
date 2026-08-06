use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rmcp::model::{CallToolRequestParams, Tool as McpToolModel};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
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

/// 单个服务器的连接（service + 已发现的工具列表）。
pub struct ServerConnection {
    service: Arc<Service>,
    tools: Vec<McpToolModel>,
}

/// MCP 服务器管理器：session 级连接缓存，
/// 懒连接 + 复用，失败记录（不自动重试，/mcp reconnect 手动），
/// 禁用名单（enable/disable 立即生效）。
pub struct McpManager {
    servers: HashMap<String, McpServerConfig>,
    disabled: HashSet<String>,
    connections: HashMap<String, ServerConnection>,
    failures: HashMap<String, String>,
}

/// 单个服务器的展示状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpStatus {
    Disabled,
    Connected { tool_count: usize },
    Failed { detail: String },
    NotConnected,
}

impl McpManager {
    pub fn new(
        servers: HashMap<String, McpServerConfig>,
        disabled: HashSet<String>,
    ) -> Self {
        Self {
            servers,
            disabled,
            connections: HashMap::new(),
            failures: HashMap::new(),
        }
    }

    pub fn configured(&self) -> Vec<String> {
        let mut names: Vec<String> = self.servers.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.contains(name)
    }

    /// 禁用名单（排序；/mcp 持久化用）。
    pub fn disabled(&self) -> Vec<String> {
        let mut names: Vec<String> = self.disabled.iter().cloned().collect();
        names.sort();
        names
    }

    pub fn status(&self, name: &str) -> McpStatus {
        if self.disabled.contains(name) {
            return McpStatus::Disabled;
        }
        if let Some(conn) = self.connections.get(name) {
            return McpStatus::Connected {
                tool_count: conn.tools.len(),
            };
        }
        if let Some(detail) = self.failures.get(name) {
            return McpStatus::Failed {
                detail: detail.clone(),
            };
        }
        McpStatus::NotConnected
    }

    /// 懒连接：连接全部未连接、未失败、未禁用的服务器。
    /// 失败记录进 failures（本轮不再重试），返回逐个结果。
    pub async fn connect_all(&mut self) -> Vec<(String, Result<(), String>)> {
        let mut results = Vec::new();
        for name in self.configured() {
            if self.connections.contains_key(&name) {
                continue;
            }
            if self.disabled.contains(&name) {
                continue;
            }
            if self.failures.contains_key(&name) {
                continue;
            }
            results.push((name.clone(), self.connect_one(&name).await));
        }
        results
    }

    async fn connect_one(&mut self, name: &str) -> Result<(), String> {
        let result = self.connect_one_inner(name).await;
        if let Err(detail) = &result {
            self.failures.insert(name.to_string(), detail.clone());
        }
        result
    }

    async fn connect_one_inner(&mut self, name: &str) -> Result<(), String> {
        let Some(config) = self.servers.get(name).cloned() else {
            return Err(format!("未配置的服务器 {name}"));
        };
        let service = match config.kind.as_deref().unwrap_or("stdio") {
            "stdio" => {
                let Some(command_str) = config.command.as_deref() else {
                    return Err("stdio 服务器缺少 command".to_string());
                };
                let mut command = TokioCommand::new(command_str);
                command.args(&config.args);
                command.envs(&config.env);
                let transport = TokioChildProcess::new(command)
                    .map_err(|e| format!("spawn {command_str}: {e}"))?;
                serve_client((), transport)
                    .await
                    .map_err(|e| format!("握手失败: {e}"))?
            }
            // Streamable HTTP（MCP 现行标准传输）
            "http" => {
                let Some(url) = config.url.as_deref() else {
                    return Err("http 服务器缺少 url".to_string());
                };
                let mut headers = HashMap::new();
                for (key, value) in &config.headers {
                    let header_name = http::HeaderName::from_bytes(key.as_bytes())
                        .map_err(|e| format!("http 头名非法 {key}: {e}"))?;
                    let header_value = http::HeaderValue::from_str(value)
                        .map_err(|e| format!("http 头 {key} 值非法: {e}"))?;
                    headers.insert(header_name, header_value);
                }
                let transport =
                    StreamableHttpClientTransport::from_config(
                        StreamableHttpClientTransportConfig::with_uri(url)
                            .custom_headers(headers),
                    );
                serve_client((), transport)
                    .await
                    .map_err(|e| format!("握手失败: {e}"))?
            }
            other => {
                return Err(format!(
                    "不支持的传输类型 {other}（支持 stdio / http；sse / ws 未落地）"
                ));
            }
        };
        let listed = service
            .list_all_tools()
            .await
            .map_err(|e| format!("list_tools 失败: {e}"))?;
        self.connections.insert(
            name.to_string(),
            ServerConnection {
                service: Arc::new(service),
                tools: listed,
            },
        );
        self.failures.remove(name);
        Ok(())
    }

    /// 重连单个服务器（先断开旧连接再连接）。
    pub async fn reconnect(&mut self, name: &str) -> Result<(), McpError> {
        self.disconnect(name);
        self.connect_one(name)
            .await
            .map_err(|detail| McpError::Connect {
                server: name.to_string(),
                detail,
            })
    }

    /// 断开连接（disable 立即生效；连接缓存与失败记录一并清除）。
    pub fn disconnect(&mut self, name: &str) {
        self.connections.remove(name);
        self.failures.remove(name);
    }

    /// 启用/禁用（禁用立即断开；启用后下一次 connect_all 懒连接生效）。
    pub fn set_enabled(&mut self, name: &str, enabled: bool) {
        if enabled {
            self.disabled.remove(name);
        } else {
            self.disabled.insert(name.to_string());
            self.disconnect(name);
        }
    }

    /// 从已连接缓存构建工具（每次回合复用，不重新 spawn）。
    pub fn tools(&self) -> Vec<Box<dyn Tool>> {
        let mut names: Vec<&String> = self.connections.keys().collect();
        names.sort();
        let mut tools = Vec::new();
        for name in names {
            if self.disabled.contains(name) {
                continue;
            }
            let conn = &self.connections[name.as_str()];
            for tool in &conn.tools {
                tools.push(Box::new(McpTool::new(
                    name,
                    tool.clone(),
                    conn.service.clone(),
                )) as Box<dyn Tool>);
            }
        }
        tools
    }
}

/// MCP 工具适配器：与内置工具共用同一 Tool trait。
pub struct McpTool {
    /// 模型可见名：mcp__{server}__{tool}（名称规范化）
    name: String,
    /// 原始服务器名（resource 块来源前缀用）。
    server_name: String,
    description: String,
    input_schema: serde_json::Value,
    tool_name: String,
    read_only: bool,
    service: Arc<Service>,
}

/// 描述上限：2048 字符。
const MAX_MCP_DESCRIPTION_LENGTH: usize = 2048;

/// 服务器/工具名规范化：`^[a-zA-Z0-9_-]{1,64}$`，非法字符（点/空格等）→ `_`。
/// 否则含点或空格的服务器名会破坏 `__` 分隔符与权限规则匹配。
pub fn normalize_mcp_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars().take(64) {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out
}

/// 从服务器工具描述派生的展示事实（纯函数，测试友好）。
pub struct McpToolFacts {
    pub name: String,
    pub server_name: String,
    pub description: String,
    pub read_only: bool,
}

impl McpTool {
    fn new(server_name: &str, tool: McpToolModel, service: Arc<Service>) -> Self {
        let facts = mcp_tool_facts(server_name, &tool);
        let tool_name = tool.name.to_string();
        Self {
            name: facts.name,
            server_name: facts.server_name,
            description: facts.description,
            input_schema: serde_json::to_value(&*tool.input_schema).unwrap_or_default(),
            tool_name,
            read_only: facts.read_only,
            service,
        }
    }
}

/// 工具展示事实派生（buildMcpToolName / normalizeNameForMCP /
/// MAX_MCP_DESCRIPTION_LENGTH / readOnlyHint 并发标记）。
pub fn mcp_tool_facts(server_name: &str, tool: &McpToolModel) -> McpToolFacts {
    let tool_name = tool.name.to_string();
    let description = tool
        .description
        .as_deref()
        .unwrap_or_default()
        .to_string();
    let description = if description.len() > MAX_MCP_DESCRIPTION_LENGTH {
        format!(
            "{}… [truncated]",
            &description[..MAX_MCP_DESCRIPTION_LENGTH]
        )
    } else {
        description
    };
    McpToolFacts {
        name: format!(
            "mcp__{}__{}",
            normalize_mcp_name(server_name),
            normalize_mcp_name(&tool_name)
        ),
        server_name: server_name.to_string(),
        description,
        read_only: tool
            .annotations
            .as_ref()
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false),
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
        // readOnlyHint 标记的工具并发安全。
        self.read_only
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        self.read_only
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
            } else if let Some(resource) = block.as_resource() {
                // resource 块带来源前缀。
                match &resource.resource {
                    rmcp::model::ResourceContents::TextResourceContents {
                        uri,
                        text: resource_text,
                        ..
                    } => {
                        text.push_str(&format!(
                            "[Resource from {} at {uri}]\n{resource_text}",
                            self.server_name
                        ));
                    }
                    rmcp::model::ResourceContents::BlobResourceContents {
                        uri,
                        blob,
                        mime_type,
                        ..
                    } => {
                        text.push_str(&format!(
                            "[Resource from {} at {uri}: blob {} bytes ({})]",
                            self.server_name,
                            blob.len(),
                            mime_type.as_deref().unwrap_or("unknown")
                        ));
                    }
                    _ => {}
                }
            } else {
                text.push_str(&format!("[{:?}]", block));
            }
            text.push('\n');
        }
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: result.is_error.unwrap_or(false),
            diff: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> HashMap<String, McpServerConfig> {
        let mut servers = HashMap::new();
        servers.insert(
            "files".to_string(),
            McpServerConfig {
                command: Some("/bin/echo".to_string()),
                args: Vec::new(),
                env: HashMap::new(),
                kind: None,
                url: None,
                headers: HashMap::new(),
            },
        );
        servers.insert(
            "web".to_string(),
            McpServerConfig {
                command: Some("/bin/false".to_string()),
                args: Vec::new(),
                env: HashMap::new(),
                kind: None,
                url: None,
                headers: HashMap::new(),
            },
        );
        servers
    }

    #[test]
    fn configured_is_sorted() {
        let mgr = McpManager::new(config(), HashSet::new());
        assert_eq!(mgr.configured(), vec!["files", "web"]);
    }

    #[test]
    fn status_matrix_without_connecting() {
        let mut mgr = McpManager::new(config(), HashSet::new());
        assert_eq!(mgr.status("files"), McpStatus::NotConnected);
        mgr.set_enabled("files", false);
        assert_eq!(mgr.status("files"), McpStatus::Disabled);
        assert_eq!(mgr.status("files"), McpStatus::Disabled);
    }

    #[test]
    fn tools_empty_without_connections() {
        let mgr = McpManager::new(config(), HashSet::new());
        assert!(mgr.tools().is_empty());
    }

    #[tokio::test]
    async fn connect_all_records_failures_without_retry() {
        let mut mgr = McpManager::new(config(), HashSet::new());
        let results = mgr.connect_all().await;
        // /bin/echo 与 /bin/false 都不是 MCP server：握手全部失败并记录。
        let failed = results
            .iter()
            .filter(|(_, r)| r.is_err())
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>();
        assert_eq!(failed, vec!["files", "web"]);
        assert!(matches!(mgr.status("files"), McpStatus::Failed { .. }));
        assert!(matches!(mgr.status("web"), McpStatus::Failed { .. }));
        // 失败不自动重试：再次 connect_all 只处理未连接未失败的。
        let second = mgr.connect_all().await;
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn reconnect_clears_failure_and_retries() {
        let mut mgr = McpManager::new(config(), HashSet::new());
        let _ = mgr.connect_all().await;
        assert!(mgr.reconnect("web").await.is_err());
        assert!(matches!(mgr.status("web"), McpStatus::Failed { .. }));
        // disconnect 清空失败记录 → NotConnected（可被下一次 connect_all 重试）。
        mgr.disconnect("web");
        assert_eq!(mgr.status("web"), McpStatus::NotConnected);
    }

    #[tokio::test]
    async fn http_server_requires_url() {
        let mut servers = HashMap::new();
        servers.insert(
            "remote".to_string(),
            McpServerConfig {
                kind: Some("http".to_string()),
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                url: None,
                headers: HashMap::new(),
            },
        );
        let mut mgr = McpManager::new(servers, HashSet::new());
        assert!(matches!(
            mgr.reconnect("remote").await,
            Err(McpError::Connect { detail, .. })
                if detail.contains("缺少 url")
        ));
    }

    #[tokio::test]
    async fn unsupported_transport_kind_reports_available_types() {
        let mut servers = HashMap::new();
        servers.insert(
            "legacy".to_string(),
            McpServerConfig {
                kind: Some("sse".to_string()),
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                url: Some("http://localhost:8000/sse".to_string()),
                headers: HashMap::new(),
            },
        );
        let mut mgr = McpManager::new(servers, HashSet::new());
        assert!(matches!(
            mgr.reconnect("legacy").await,
            Err(McpError::Connect { detail, .. })
                if detail.contains("stdio / http") && detail.contains("sse")
        ));
    }

    #[test]
    fn parses_http_server_config() {
        let cfg: McpServerConfig = serde_json::from_value(serde_json::json!({
            "type": "http",
            "url": "https://mcp.example.com/mcp",
            "headers": { "Authorization": "Bearer token" },
        }))
        .unwrap();
        assert_eq!(cfg.kind.as_deref(), Some("http"));
        assert_eq!(cfg.command, None);
        assert_eq!(cfg.url.as_deref(), Some("https://mcp.example.com/mcp"));
        assert_eq!(
            cfg.headers.get("Authorization").map(String::as_str),
            Some("Bearer token")
        );
        // stdio 配置（旧格式）仍可解析：command 非必填字段。
        let stdio: McpServerConfig = serde_json::from_value(serde_json::json!({
            "command": "npx",
            "args": ["-y", "mcp-server"],
        }))
        .unwrap();
        assert_eq!(stdio.kind, None);
        assert_eq!(stdio.command.as_deref(), Some("npx"));
    }

    #[test]
    fn disconnect_clears_connection_and_failure() {
        let mut mgr = McpManager::new(config(), HashSet::new());
        mgr.failures.insert("web".to_string(), "boom".to_string());
        mgr.disconnect("web");
        assert_eq!(mgr.status("web"), McpStatus::NotConnected);
    }

    #[test]
    fn normalize_mcp_name_maps_invalid_chars() {
        assert_eq!(normalize_mcp_name("my.server"), "my_server");
        assert_eq!(normalize_mcp_name("my-server_1"), "my-server_1");
        assert_eq!(normalize_mcp_name("a b.c"), "a_b_c");
        // 64 字符上限。
        assert_eq!(
            normalize_mcp_name(&"x".repeat(80)).len(),
            64
        );
    }

    fn tool_model(name: &str, description: Option<&str>, read_only: bool) -> McpToolModel {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "description": description,
            "inputSchema": {"type": "object"},
            "annotations": if read_only {
                serde_json::json!({"readOnlyHint": true})
            } else {
                serde_json::json!(null)
            },
        }))
        .unwrap()
    }

    #[test]
    fn mcp_tool_facts_normalized_and_readonly_hint() {
        let facts = mcp_tool_facts(
            "my server",
            &tool_model("read file", Some("d"), true),
        );
        assert_eq!(facts.name, "mcp__my_server__read_file");
        assert_eq!(facts.server_name, "my server");
        assert!(facts.read_only);
    }

    #[test]
    fn mcp_tool_facts_description_truncated() {
        let long = "d".repeat(3000);
        let facts = mcp_tool_facts("srv", &tool_model("t", Some(&long), false));
        assert!(facts.description.ends_with("… [truncated]"));
        assert!(facts.description.len() < 2048 + 20);
        assert!(!facts.read_only);
    }

    #[test]
    fn set_enabled_toggles_disabled_list() {
        let mut mgr = McpManager::new(config(), HashSet::new());
        mgr.set_enabled("files", false);
        assert_eq!(mgr.status("files"), McpStatus::Disabled);
        mgr.set_enabled("files", true);
        assert_ne!(mgr.status("files"), McpStatus::Disabled);
    }
}
