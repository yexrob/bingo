use crate::tool::bash::BashTool;
use crate::tool::read::ReadTool;
use crate::tool::Tool;

/// 基础工具池（对标 getAllBaseTools 的最小面）。
pub async fn assemble_tools(
    mcp_servers: &std::collections::HashMap<String, crate::settings::McpServerConfig>,
) -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> =
        vec![Box::new(BashTool::new()), Box::new(ReadTool::new())];
    match crate::mcp::connect_servers(mcp_servers).await {
        Ok(mcp_tools) => tools.extend(mcp_tools),
        Err(e) => eprintln!("[bingo] MCP warning: {e}"),
    }
    tools
}
