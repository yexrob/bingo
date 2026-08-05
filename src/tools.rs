use std::sync::Arc;

use crate::query::Session;
use crate::tool::agent::AgentTool;
use crate::tool::bash::BashTool;
use crate::tool::edit::EditTool;
use crate::tool::glob::GlobTool;
use crate::tool::grep::GrepTool;
use crate::tool::read::ReadTool;
use crate::tool::task::{TaskCreateTool, TaskGetTool, TaskListTool, TaskUpdateTool};
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
        Box::new(TaskCreateTool),
        Box::new(TaskUpdateTool),
        Box::new(TaskGetTool),
        Box::new(TaskListTool),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn assembles_task_tools() {
        let session = std::sync::Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "https://example.com".into()),
            model: "m".into(),
            permission_mode: crate::permission::PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            transcript: None,
            depth: 0,
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                &std::env::temp_dir(),
            )),
            last_task_reminder_turn: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
        });
        let mut warn = |_: String| {};
        let tools = assemble_tools(&session, &mut warn).await;
        let names: Vec<String> = tools.iter().map(|t| t.name()).collect();
        for expected in ["TaskCreate", "TaskUpdate", "TaskGet", "TaskList"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}: {names:?}");
        }
    }
}
