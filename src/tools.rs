use std::sync::Arc;

use crate::query::Session;
use crate::tool::agent::AgentTool;
use crate::tool::bash::BashTool;
use crate::tool::edit::EditTool;
use crate::tool::glob::GlobTool;
use crate::tool::grep::GrepTool;
use crate::tool::read::ReadTool;
use crate::tool::webfetch::WebFetchTool;
use crate::tool::websearch::WebSearchTool;
use crate::tool::write::WriteTool;
use crate::tool::Tool;

/// 基础工具池（对标 getAllBaseTools 的最小面）+ MCP + 子代理。
pub async fn assemble_tools(
    session: &Arc<Session>,
    on_warning: &mut (dyn Fn(String) + Send),
) -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(BashTool::new()),
        Box::new(ReadTool::new()),
        Box::new(GlobTool),
        Box::new(GrepTool),
        Box::new(EditTool),
        Box::new(WriteTool),
        Box::new(WebFetchTool),
        Box::new(WebSearchTool),
        Box::new(AgentTool::new(session.clone())),
    ];
    match crate::mcp::connect_servers(&session.settings.mcp_servers).await {
        Ok(mcp_tools) => {
            if !session.quiet && !mcp_tools.is_empty() {
                eprintln!("[bingo] connected {} MCP tools", mcp_tools.len());
            }
            tools.extend(mcp_tools);
        }
        Err(e) => on_warning(format!("MCP: {e}")),
    }
    tools
}
