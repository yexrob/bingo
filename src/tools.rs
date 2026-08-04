use std::sync::Arc;

use crate::query::Session;
use crate::tool::agent::AgentTool;
use crate::tool::bash::BashTool;
use crate::tool::read::ReadTool;
use crate::tool::Tool;

/// 基础工具池（对标 getAllBaseTools 的最小面）+ MCP + 子代理。
pub async fn assemble_tools(session: &Arc<Session>) -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(BashTool::new()),
        Box::new(ReadTool::new()),
        Box::new(AgentTool::new(session.clone())),
    ];
    match crate::mcp::connect_servers(&session.settings.mcp_servers).await {
        Ok(mcp_tools) => tools.extend(mcp_tools),
        Err(e) => eprintln!("[bingo] MCP warning: {e}"),
    }
    tools
}
