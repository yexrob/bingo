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

use crate::error::ErrorCode;
use crate::settings::McpServerConfig;
use crate::tool::{Tool, ToolContext, ToolError, ToolResult};

#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP server {server}: {detail}")]
    Connect { server: String, detail: String },
}

impl ErrorCode for McpError {
    fn error_code(&self) -> &'static str {
        match self {
            McpError::Connect { .. } => "SERVER_ERROR",
        }
    }
}

type Service = RunningService<RoleClient, ()>;

/// Connection to a single server (service + the discovered tool list).
pub struct ServerConnection {
    service: Arc<Service>,
    tools: Vec<McpToolModel>,
}

/// Per-server connect timeout: a bad server (handshake hanging) occupies the
/// background task for at most a few seconds and never blocks turn input.
/// Timeouts are recorded as failures; retry manually via /mcp reconnect.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// MCP server manager: session-level connection cache,
/// lazy connect + reuse, failures recorded (no auto-retry; manual via /mcp reconnect),
/// disabled list (enable/disable takes effect immediately).
pub struct McpManager {
    servers: HashMap<String, McpServerConfig>,
    disabled: HashSet<String>,
    connections: HashMap<String, ServerConnection>,
    failures: HashMap<String, String>,
    /// Background connection in flight (prevents duplicate spawns across
    /// concurrent turns; never blocks the executor itself).
    connecting: HashSet<String>,
    /// Failures already reported to the UI (background connection failures
    /// are reported one turn later, once per server).
    reported: HashSet<String>,
    /// Per-server connect timeout (default 5s; tests may shorten it).
    connect_timeout: std::time::Duration,
}

/// Display status of a single server.
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
            connecting: HashSet::new(),
            reported: HashSet::new(),
            connect_timeout: CONNECT_TIMEOUT,
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

    /// Disabled list (sorted; used by /mcp persistence).
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

    /// Lazy connect: connect every server that is not connected, not failed, and not disabled.
    /// The `connecting` marker only prevents duplicate spawns, never blocks
    /// execution — the caller is the only executor.
    /// Failures go into `failures` (no retry this round); returns per-server results.
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
        let result = match tokio::time::timeout(self.connect_timeout, self.connect_one_inner(name)).await {
            Ok(result) => result,
            Err(_) => Err(format!("连接超时（{}s）", self.connect_timeout.as_secs())),
        };
        if let Err(detail) = &result {
            self.failures.insert(name.to_string(), detail.clone());
        }
        result
    }

    /// Servers awaiting background connection (not connected, not failed,
    /// not disabled, not already in flight).
    pub fn needs_connect(&self) -> Vec<String> {
        let mut names = Vec::new();
        for name in self.configured() {
            if self.connections.contains_key(&name)
                || self.disabled.contains(&name)
                || self.failures.contains_key(&name)
                || self.connecting.contains(&name)
            {
                continue;
            }
            names.push(name);
        }
        names
    }

    /// Mark as in flight (called before assemble_tools spawns; prevents
    /// duplicate connects across concurrent turns).
    pub fn mark_connecting(&mut self, names: &[String]) {
        for name in names {
            self.connecting.insert(name.clone());
        }
    }

    /// Background connect finished (outcome recorded in
    /// connections/failures); clears the in-flight marker.
    pub fn finish_connecting(&mut self, names: &[String]) {
        for name in names {
            self.connecting.remove(name);
        }
    }

    /// Failures not yet reported → display text and mark (once per
    /// server, until disconnect).
    pub fn drain_unreported_failures(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for name in self.configured() {
            if self.reported.contains(&name) {
                continue;
            }
            if let Some(detail) = self.failures.get(&name) {
                self.reported.insert(name.clone());
                out.push(format!("MCP {name}: {detail}"));
            }
        }
        out
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
                // If the child process's stderr inherits the terminal it writes straight
                // through the TUI (scrollback is never redrawn, one log line stays on
                // screen forever) — redirect it to a log file instead.
                // Must go through the builder: TokioChildProcess::new overrides the
                // stderr already set on the Command with the default Stdio::inherit at spawn.
                let (transport, _stderr) = TokioChildProcess::builder(command)
                    .stderr(stderr_sink(name))
                    .spawn()
                    .map_err(|e| format!("spawn {command_str}: {e}"))?;
                serve_client((), transport)
                    .await
                    .map_err(|e| format!("握手失败: {e}"))?
            }
            // Streamable HTTP (the current standard MCP transport)
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

    /// Reconnect a single server (disconnect the old connection first, then connect).
    pub async fn reconnect(&mut self, name: &str) -> Result<(), McpError> {
        self.disconnect(name);
        self.connect_one(name)
            .await
            .map_err(|detail| McpError::Connect {
                server: name.to_string(),
                detail,
            })
    }

    /// Disconnect (disable takes effect immediately; clears the connection cache,
    /// failure records and reported marks, so a new failure after
    /// reconnect/disable is reported again).
    pub fn disconnect(&mut self, name: &str) {
        self.connections.remove(name);
        self.failures.remove(name);
        self.connecting.remove(name);
        self.reported.remove(name);
    }

    /// Enable/disable (disable disconnects immediately; enabling takes effect lazily on the next connect_all).
    pub fn set_enabled(&mut self, name: &str, enabled: bool) {
        if enabled {
            self.disabled.remove(name);
        } else {
            self.disabled.insert(name.to_string());
            self.disconnect(name);
        }
    }

    /// Build tools from the connected cache (reused every turn; no re-spawn).
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

/// MCP tool adapter: shares the same Tool trait with built-in tools.
pub struct McpTool {
    /// Model-visible name: mcp__{server}__{tool} (normalized name)
    name: String,
    /// Original server name (source prefix for resource blocks).
    server_name: String,
    description: String,
    input_schema: serde_json::Value,
    tool_name: String,
    read_only: bool,
    service: Arc<Service>,
}

/// Description cap: 2048 chars (character count, not bytes).
const MAX_MCP_DESCRIPTION_LENGTH: usize = 2048;

/// Server/tool name normalization: `^[a-zA-Z0-9_-]{1,64}$`; invalid chars (dots, spaces,
/// etc.) → `_`. Otherwise server names with dots or spaces would break the `__` separator
/// and permission-rule matching.
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

/// stderr destination of stdio servers: `~/.local/share/bingo/logs/mcp-<name>.log`
/// (truncated and rewritten on each connect); if the file can't be opened, drop it —
/// never inherit the terminal.
fn stderr_sink(name: &str) -> std::process::Stdio {
    stderr_log_file(name).map_or_else(std::process::Stdio::null, std::process::Stdio::from)
}

fn stderr_log_file(name: &str) -> Option<std::fs::File> {
    let home = crate::platform::home_dir();
    if home.as_os_str().is_empty() {
        return None;
    }
    let path = mcp_log_path(&home, name);
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::File::create(path).ok()
}

/// Log file path (pure function, easy to test). The file name goes through
/// [`normalize_mcp_name`], the same scheme as tool-name prefixes.
fn mcp_log_path(home: &std::path::Path, name: &str) -> std::path::PathBuf {
    home.join(".local")
        .join("share")
        .join("bingo")
        .join("logs")
        .join(format!("mcp-{}.log", normalize_mcp_name(name)))
}

#[cfg(test)]
mod stderr_log_tests {
    use super::*;

    #[test]
    fn stderr_log_path_is_per_server_and_sanitized() {
        let path = mcp_log_path(std::path::Path::new("/home/u"), "files v2");
        assert_eq!(
            path,
            std::path::Path::new("/home/u/.local/share/bingo/logs/mcp-files_v2.log")
        );
    }
}

/// Display facts derived from a server tool description (pure function, test-friendly).
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

/// Tool display-facts derivation (buildMcpToolName / normalizeNameForMCP /
/// MAX_MCP_DESCRIPTION_LENGTH / readOnlyHint concurrency marker).
pub fn mcp_tool_facts(server_name: &str, tool: &McpToolModel) -> McpToolFacts {
    let tool_name = tool.name.to_string();
    let description = tool
        .description
        .as_deref()
        .unwrap_or_default()
        .to_string();
    // Byte-index slicing would panic mid-multibyte character (Chinese/emoji descriptions):
    // truncate by chars instead.
    let description = if description.chars().count() > MAX_MCP_DESCRIPTION_LENGTH {
        let head: String = description.chars().take(MAX_MCP_DESCRIPTION_LENGTH).collect();
        format!("{head}… [truncated]")
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
        // Tools marked readOnlyHint are concurrency-safe.
        self.read_only
    }

    /// Note: readOnlyHint is untrusted input self-reported by the server — only for
    /// concurrency scheduling, never for the permission gate (see
    /// permission::can_use_tool's handling of mcp__ tools).
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
                // Resource blocks carry a source prefix.
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
        // Neither /bin/echo nor /bin/false is an MCP server: every handshake fails and is recorded.
        let failed = results
            .iter()
            .filter(|(_, r)| r.is_err())
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>();
        assert_eq!(failed, vec!["files", "web"]);
        assert!(matches!(mgr.status("files"), McpStatus::Failed { .. }));
        assert!(matches!(mgr.status("web"), McpStatus::Failed { .. }));
        // No auto-retry on failure: the next connect_all only handles not-connected, not-failed ones.
        let second = mgr.connect_all().await;
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn reconnect_clears_failure_and_retries() {
        let mut mgr = McpManager::new(config(), HashSet::new());
        let _ = mgr.connect_all().await;
        assert!(mgr.reconnect("web").await.is_err());
        assert!(matches!(mgr.status("web"), McpStatus::Failed { .. }));
        // disconnect clears the failure record → NotConnected (retryable by the next connect_all).
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
        // The stdio config (legacy format) still parses: command is not a required field.
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

    /// The connect list excludes connected/failed/disabled/in-flight
    /// servers (prevents duplicate spawns).
    #[test]
    fn needs_connect_excludes_done_failed_disabled_inflight() {
        let mut mgr = McpManager::new(config(), HashSet::new());
        assert_eq!(mgr.needs_connect(), vec!["files", "web"]);

        mgr.set_enabled("files", false);
        assert_eq!(mgr.needs_connect(), vec!["web"]);

        mgr.failures.insert("web".to_string(), "boom".to_string());
        assert!(mgr.needs_connect().is_empty());

        mgr.disconnect("web");
        mgr.mark_connecting(&["web".to_string()]);
        assert!(mgr.needs_connect().is_empty(), "进行中不重复派发");
        mgr.finish_connecting(&["web".to_string()]);
        assert_eq!(mgr.needs_connect(), vec!["web"]);
    }

    /// Background connect failures are reported with delay: once per
    /// server, reset on disconnect.
    #[test]
    fn unreported_failures_drain_once_until_disconnect() {
        let mut mgr = McpManager::new(config(), HashSet::new());
        mgr.failures.insert("files".to_string(), "boom".to_string());
        mgr.failures.insert("web".to_string(), "nope".to_string());

        let first = mgr.drain_unreported_failures();
        assert_eq!(first.len(), 2, "两条失败都报告");
        assert!(first.iter().any(|w| w.contains("files") && w.contains("boom")));
        assert!(mgr.drain_unreported_failures().is_empty(), "只报一次");

        // disconnect resets the reported marks: a new failure after
        // reconnect can be reported again.
        mgr.disconnect("files");
        mgr.failures.insert("files".to_string(), "boom".to_string());
        let again = mgr.drain_unreported_failures();
        assert_eq!(again.len(), 1, "disconnect 后新失败可再报告");
        assert!(again[0].contains("files"));
    }

    /// A server whose handshake hangs: recorded as a failure on timeout,
    /// never blocking indefinitely.
    #[tokio::test]
    async fn hung_server_times_out_and_records_failure() {
        let mut servers = HashMap::new();
        // A long-running dummy server: /bin/sleep on Unix, ping on Windows (no /bin/sleep).
        #[cfg(windows)]
        let (command, args) = ("ping".to_string(), vec!["-n".to_string(), "10".to_string(), "127.0.0.1".to_string()]);
        #[cfg(not(windows))]
        let (command, args) = ("/bin/sleep".to_string(), vec!["10".to_string()]);
        servers.insert(
            "hung".to_string(),
            McpServerConfig {
                command: Some(command),
                args,
                env: HashMap::new(),
                kind: None,
                url: None,
                headers: HashMap::new(),
            },
        );
        let mut mgr = McpManager::new(servers, HashSet::new());
        mgr.connect_timeout = std::time::Duration::from_millis(200);
        let results = mgr.connect_all().await;
        assert!(matches!(results.as_slice(), [(name, Err(detail))] if name == "hung" && detail.contains("连接超时")));
        assert!(matches!(mgr.status("hung"), McpStatus::Failed { .. }));
    }

    #[test]
    fn normalize_mcp_name_maps_invalid_chars() {
        assert_eq!(normalize_mcp_name("my.server"), "my_server");
        assert_eq!(normalize_mcp_name("my-server_1"), "my-server_1");
        assert_eq!(normalize_mcp_name("a b.c"), "a_b_c");
        // 64-char cap.
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
        assert_eq!(facts.description.chars().count(), MAX_MCP_DESCRIPTION_LENGTH + 13);
        assert!(!facts.read_only);
    }

    /// Regression: Chinese/emoji descriptions used to be sliced by bytes at a non-char
    /// boundary → panic.
    #[test]
    fn mcp_tool_facts_truncates_multibyte_description_on_char_boundary() {
        for unit in ["中", "🙂", "é"] {
            let long = unit.repeat(3000);
            let facts = mcp_tool_facts("srv", &tool_model("t", Some(&long), false));
            assert!(facts.description.ends_with("… [truncated]"), "{unit}");
            assert_eq!(
                facts.description.chars().count(),
                MAX_MCP_DESCRIPTION_LENGTH + 13,
                "{unit}"
            );
        }
        // Exactly at the cap: no truncation.
        let exact = "中".repeat(MAX_MCP_DESCRIPTION_LENGTH);
        let facts = mcp_tool_facts("srv", &tool_model("t", Some(&exact), false));
        assert_eq!(facts.description, exact);
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
